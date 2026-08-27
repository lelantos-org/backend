//! Live-tail worker handler.
//!
//! Ticks, sleeps and survives transient failures. Propagating a provider error
//! straight out of the worker task would stop that chain until the process
//! restarted, so ticks are retried under a per-tick policy.
//!
//! # Pacing
//!
//! `block_poll_ms` is the ceiling of an idle backoff, not a fixed period. A tick
//! that left known work behind loops straight back; a tick that caught up waits,
//! starting at the [`Backoff::idle`] floor and doubling. Sleeping the full
//! interval after every tick put the whole poll period between a block landing
//! and its rows appearing, whether or not the chain was busy. This is the pacing
//! `shared::tick` gives every other indexer in the workspace; the driver itself
//! does not fit here, because a tick must be able to fail the worker (releasing
//! the chain lock for a standby) and to hand back [`LiveExit::Lagging`].

use crate::domain::error::IngesterError;
use crate::domain::models::TickOutcome;
use crate::services::live::LiveService;
use crate::services::retry::{Policy, retrying};
use shared::backoff::Backoff;
use std::sync::Arc;
use tracing::{debug, info};

/// Why the live loop handed control back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveExit {
    /// Fell far enough behind that backfill should take over again.
    Lagging { lag: i64 },
}

pub async fn run(svc: Arc<dyn LiveService>) -> Result<LiveExit, IngesterError> {
    let chain_id = svc.chain_id();
    let mut backoff = Backoff::idle(svc.poll_ms());
    info!(chain_id, poll_ms = svc.poll_ms(), "live mode start");

    loop {
        // Retries are per tick, so a chain that recovers does not carry a failure
        // budget forward. Exhausting them returns the error, releasing the
        // advisory lock so a standby can take the chain.
        let outcome = retrying(Policy::LIVE_TICK, "live tick", chain_id, || svc.tick()).await?;

        if let TickOutcome::Lagging { lag } = outcome {
            info!(
                chain_id,
                lag, "lag exceeds threshold; returning to backfill"
            );
            return Ok(LiveExit::Lagging { lag });
        }

        // Known work left over means loop straight back; anything else means the
        // chain is caught up as far as this tick could see, so let the delay grow.
        let queued = match outcome {
            // The rewound range must be re-scanned. Bounded: a rewind that finds
            // no survivor clears the anchor, so the next tick cannot detect a
            // fork again and falls through to a scan.
            TickOutcome::Reorg { .. } => true,
            // Only when the scan was capped short of the tip. A scan that reached
            // it has nothing waiting, and looping would spend a cursor read and
            // two RPCs to be told so.
            TickOutcome::Committed { reached_tip, .. } | TickOutcome::Empty { reached_tip, .. } => {
                backoff.reset();
                !reached_tip
            }
            TickOutcome::Idle | TickOutcome::Lagging { .. } => false,
        };

        if queued {
            // A yield, not a delay: this arm awaits nothing else, and the worker
            // selects this future against shutdown and lock-loss. Without a
            // suspension point a chain catching up would never let those arms be
            // polled.
            tokio::task::yield_now().await;
            continue;
        }

        let delay = backoff.next_delay();
        debug!(
            chain_id,
            ?outcome,
            delay_ms = delay.as_millis() as u64,
            "live tick idle"
        );
        tokio::time::sleep(delay).await;
    }
}
