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

use diesel::sql_types::Text;
use diesel::{QueryResult, sql_query};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::StreamExt;
use futures::stream;
use shared::backoff::Backoff;
use std::time::Duration;
use tokio::sync::watch;
use tokio_postgres::{AsyncMessage, Client, NoTls};
use tracing::{debug, info, warn};

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

/// Floor of the reconnect backoff.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
/// Ceiling of the reconnect backoff. Beyond this the poll alone sustains the
/// service, so more frequent retries add nothing.
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const RECONNECT_FACTOR: u32 = 2;

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
    tokio::spawn(async move { run(url, channels, tx).await });
    rx
}

async fn run(url: String, channels: &'static [&'static str], tx: watch::Sender<u64>) {
    let mut backoff = Backoff::new(RECONNECT_MIN, RECONNECT_MAX, RECONNECT_FACTOR);
    loop {
        // A closed receiver means every consumer is gone; nothing left to wake.
        if tx.is_closed() {
            return;
        }
        match session(&url, channels, &tx).await {
            // Subscribed successfully before the stream ended, so the database
            // is reachable: start the next backoff from the floor.
            Ok(()) => {
                warn!(?channels, "listen connection closed; reconnecting");
                backoff.reset();
            }
            Err(e) => warn!(?channels, error = %e, "listen session failed; reconnecting"),
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// One connection's lifetime: subscribe, then forward notifications until the
/// stream ends or errors.
async fn session(
    url: &str,
    channels: &'static [&'static str],
    tx: &watch::Sender<u64>,
) -> Result<(), tokio_postgres::Error> {
    // `NoTls` matches how diesel-async establishes every pooled connection, so
    // this needs no transport configuration of its own.
    let (client, mut connection) = tokio_postgres::connect(url, NoTls).await?;

    // Notifications arrive on the connection rather than the client, and only
    // while something polls it; `poll_message` is the only accessor that
    // surfaces them.
    //
    // `subscribing` is therefore raced against the message stream rather than
    // awaited first: its statements travel on this connection, so awaiting them
    // while nothing drives it would deadlock.
    let mut messages = stream::poll_fn(move |cx| connection.poll_message(cx));
    let subscribing = subscribe(&client, channels);
    tokio::pin!(subscribing);
    let mut subscribed = false;

    loop {
        tokio::select! {
            result = &mut subscribing, if !subscribed => {
                result?;
                subscribed = true;
                info!(?channels, "listening");
                // The window before the subscription went live is uncovered;
                // one wake closes it at the cost of a single tick.
                bump(tx);
            }
            message = messages.next() => match message {
                Some(Ok(AsyncMessage::Notification(n))) => {
                    debug!(channel = n.channel(), payload = n.payload(), "notify");
                    bump(tx);
                }
                // Server-side notices (warnings, `client_min_messages`
                // output): neither a wake nor a reason to drop the connection.
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e),
                // The connection ended cleanly.
                None => return Ok(()),
            },
        }
    }
}

/// Issue one `LISTEN` per channel.
///
/// `LISTEN` takes an identifier and cannot be parameterised. Interpolation is
/// safe here because the names are compile-time constants from this module and
/// never caller input.
async fn subscribe(client: &Client, channels: &[&str]) -> Result<(), tokio_postgres::Error> {
    for channel in channels {
        client.batch_execute(&format!("LISTEN {channel}")).await?;
    }
    Ok(())
}

/// Signal every consumer that work may be waiting.
///
/// Only the change of the counter is significant; its value carries no meaning.
fn bump(tx: &watch::Sender<u64>) {
    tx.send_modify(|n| *n += 1);
}
