//! Postgres `LISTEN` as a wake source for polling loops.
//!
//! The indexers are correct on their poll alone: every consumer tracks a
//! durable cursor, so an undelivered notification costs latency and nothing
//! else. This module only collapses that latency, so every failure path here
//! degrades to the next poll rather than surfacing an error.
//!
//! Like [`crate::advisory`], this holds a dedicated connection that never
//! enters the bb8 pool. A pooled connection is returned after its query and
//! eventually reaped by `idle_timeout`, cancelling the `LISTEN` while the
//! process still considers itself subscribed.

mod session;

use diesel::sql_types::Text;
use diesel::{QueryResult, sql_query};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use tokio::sync::watch;
use tracing::warn;

/// Channel announcing newly appended `raw_events` rows. Payload is the
/// `chain_id` in decimal.
pub const CHANNEL_RAW_EVENTS_APPENDED: &str = "raw_events_appended";

/// Channel announcing that `raw_events` rows at or above a height were
/// withdrawn.
///
/// Consumers stream `raw_events` by ascending `id`, so re-inserted canonical
/// rows are picked up by the cursor, but state already derived from the
/// orphaned rows must be retracted explicitly. Payload is
/// `<chain_id>:<rewind_to>`.
///
/// A latency optimisation only: `chain_reorgs` is the durable record, since a
/// NOTIFY sent while a consumer is down is lost.
pub const CHANNEL_RAW_EVENTS_REORG: &str = "raw_events_reorg";

/// Channel announcing newly committed `notes` rows. Payload is the `chain_id`
/// in decimal.
pub const CHANNEL_NOTES_APPENDED: &str = "notes_appended";

/// Publish on `channel` over `conn`, waking every listener subscribed to it.
///
/// Mechanism only, no policy: Postgres queues a `NOTIFY` until its transaction
/// commits, so calling this inside the transaction that writes the rows makes
/// the wake exactly as durable as the rows it announces, and a rollback
/// announces nothing. That is the preferred shape, and it costs no connection of
/// its own.
///
/// Use [`notify_best_effort`] when the write has already committed — on separate
/// connections, or across several of them — and there is no transaction left to
/// ride.
pub async fn notify(conn: &mut AsyncPgConnection, channel: &str, payload: &str) -> QueryResult<()> {
    sql_query("SELECT pg_notify($1, $2)")
        .bind::<Text, _>(channel)
        .bind::<Text, _>(payload)
        .execute(conn)
        .await?;
    Ok(())
}

/// Publish on `channel` from the pool, logging rather than returning a failure.
///
/// For producers whose rows are already committed, where returning an error
/// would ask the caller to fail a batch it cannot roll back. Every consumer's
/// cursor is durable, so a wake that never arrives costs latency and nothing
/// else — which is what makes swallowing the error correct here and wrong
/// inside a transaction.
///
/// Takes the pool rather than a connection: this shape needs a checkout of its
/// own, and having it here keeps the checkout and the "log and continue" from
/// being restated at each producer.
pub async fn notify_best_effort(pool: &crate::DbPool, channel: &str, payload: &str) {
    let result = match pool.get().await {
        Ok(mut conn) => notify(&mut conn, channel, payload)
            .await
            .err()
            .map(|e| e.to_string()),
        Err(e) => Some(e.to_string()),
    };
    if let Some(error) = result {
        warn!(channel, payload, %error, "notify failed after a successful commit");
    }
}

/// Re-exported so a caller wiring a listener into a tick driver refers to a
/// single type.
pub use shared::tick::Wake;

/// `LISTEN` on `channels`, bumping the returned watch on every notification.
///
/// Never fails: the first connection is established by the spawned task, so an
/// unavailable database delays the first wake instead of failing the caller's
/// startup. Reconnects on backoff for the life of the process.
///
/// A reconnect bumps unconditionally. Postgres does not replay notifications
/// sent while the socket was down, so bumping converts that gap into one
/// spurious wake rather than a wait for the consumer's idle ceiling.
pub fn spawn(database_url: &str, channels: &'static [&'static str]) -> Wake {
    let (tx, rx) = watch::channel(0u64);
    // `LISTEN` is session state, so it must not be multiplexed by a pooler.
    // See `crate::direct`.
    let url = crate::direct::url(database_url);
    tokio::spawn(async move { session::run(url, channels, tx).await });
    rx
}
