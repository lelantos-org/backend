//! Postgres `LISTEN` as a wake source for polling loops.
//!
//! The indexers are correct on their poll alone: every consumer tracks a
//! durable cursor, so a notification that is never delivered costs latency and
//! nothing else. This exists purely to collapse that latency, which is why
//! every failure path here degrades to "the poll will find it" rather than
//! surfacing an error.
//!
//! Like [`crate::advisory`], this holds a **dedicated connection that never
//! enters the bb8 pool**. A pooled connection is returned after its query and
//! eventually reaped by `idle_timeout`, which would silently cancel the
//! `LISTEN` while the process kept believing it was subscribed.

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
/// rows are picked up naturally — but state they already derived from the
/// orphaned rows is invisible to that cursor and has to be retracted
/// explicitly. Payload is `<chain_id>:<rewind_to>`.
///
/// This is a latency optimisation, not the mechanism: `chain_reorgs` is the
/// durable record, because a NOTIFY sent while a consumer is down is lost.
pub const CHANNEL_RAW_EVENTS_REORG: &str = "raw_events_reorg";

/// Channel announcing newly committed `notes` rows. Payload is the `chain_id`
/// in decimal.
pub const CHANNEL_NOTES_APPENDED: &str = "notes_appended";

/// Publish on `channel`, waking every listener subscribed to it.
///
/// Best-effort by contract at every call site: the rows are already committed
/// and the consumer's durable cursor finds them on its next poll regardless, so
/// a caller logs a failure and carries on rather than failing a batch over a
/// lost optimisation.
///
/// Takes a connection rather than the pool so the caller keeps its own error
/// mapping and can, if it ever needs to, publish inside its own transaction.
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
/// Ceiling of the reconnect backoff. Past this the poll is carrying the
/// service anyway, so retrying harder buys nothing.
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const RECONNECT_FACTOR: u32 = 2;

/// Re-exported so a caller wiring a listener into a tick driver names one
/// type, not two identical ones.
pub use shared::tick::Wake;

/// `LISTEN` on `channels`, bumping the returned watch on every notification.
///
/// Never fails: the first connection is established by the spawned task, so a
/// database that is not up yet delays the first wake instead of failing the
/// caller's startup. Reconnects on backoff for the life of the process.
///
/// # Why a reconnect bumps unconditionally
///
/// Notifications sent while the socket was down are gone — Postgres does not
/// replay them. Bumping on reconnect turns that hole into one spurious wake
/// (an immediate tick that may find nothing) instead of a silent wait for the
/// consumer's idle ceiling.
pub fn spawn(database_url: &str, channels: &'static [&'static str]) -> Wake {
    let (tx, rx) = watch::channel(0u64);
    let url = database_url.to_string();
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
            // Subscribed successfully and the stream ended afterwards, so the
            // database is reachable — start the next backoff from the floor.
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

    // Notifications arrive on the *connection*, not the client, and only while
    // something polls it — `poll_message` is the only accessor that surfaces
    // them at all.
    //
    // That is also why `subscribing` is raced against the message stream rather
    // than simply awaited first: its statements travel on this connection, so
    // awaiting them while nothing drives it deadlocks on a reply that can
    // never arrive.
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
                // The subscription is live, but the window before it was not
                // covered. One wake now costs a single tick and closes it.
                bump(tx);
            }
            message = messages.next() => match message {
                Some(Ok(AsyncMessage::Notification(n))) => {
                    debug!(channel = n.channel(), payload = n.payload(), "notify");
                    bump(tx);
                }
                // Server-side notices (warnings, `client_min_messages`
                // output). Not a wake, and not a reason to drop the connection.
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
/// The names are compile-time constants from this module, never caller input,
/// so interpolating them is safe — `LISTEN` takes an identifier and cannot be
/// parameterised.
async fn subscribe(client: &Client, channels: &[&str]) -> Result<(), tokio_postgres::Error> {
    for channel in channels {
        client.batch_execute(&format!("LISTEN {channel}")).await?;
    }
    Ok(())
}

/// Signal every consumer that something may be waiting for them.
///
/// The counter only has to change; its value carries no meaning.
fn bump(tx: &watch::Sender<u64>) {
    tx.send_modify(|n| *n += 1);
}
