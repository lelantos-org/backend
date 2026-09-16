//! The subscriber's connection: subscribe, forward notifications, reconnect.

use futures::StreamExt;
use futures::stream;
use shared::backoff::Backoff;
use std::time::Duration;
use tokio::sync::watch;
use tokio_postgres::{AsyncMessage, Client, NoTls};
use tracing::{debug, info, warn};

/// Floor of the reconnect backoff.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
/// Ceiling of the reconnect backoff. Beyond this the poll alone sustains the
/// service, so more frequent retries add nothing.
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const RECONNECT_FACTOR: u32 = 2;

pub(super) async fn run(url: String, channels: &'static [&'static str], tx: watch::Sender<u64>) {
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
