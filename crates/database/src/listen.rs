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

/// Publish on `channel`, waking every listener subscribed to it.
///
/// Best-effort by contract at every call site: the rows are already committed
/// and the consumer's durable cursor finds them on its next poll, so callers
/// log a failure and continue rather than failing the batch.
///
/// Takes a connection rather than the pool so the caller keeps its own error
/// mapping and can publish inside its own transaction.
pub async fn notify(conn: &mut AsyncPgConnection, channel: &str, payload: &str) -> QueryResult<()> {
    sql_query("SELECT pg_notify($1, $2)")
        .bind::<Text, _>(channel.to_string())
        .bind::<Text, _>(payload.to_string())
        .execute(conn)
        .await?;
    Ok(())
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
