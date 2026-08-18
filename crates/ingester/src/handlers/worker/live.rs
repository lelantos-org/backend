//! Live-tail worker handler.
//!
//! Tick, sleep, and — the part that used to be missing — survive a failure.
//! A bare `svc.tick().await?` propagated any transient provider error all the
//! way out of the worker task, and nothing restarted it: one 429 stopped that
//! chain until the process was restarted.

use crate::domain::error::IngesterError;
use crate::domain::models::TickOutcome;
use crate::services::live::LiveService;
use crate::services::retry::{Policy, retrying};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Why the live loop handed control back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveExit {
    /// Fell far enough behind that backfill should take over again.
    Lagging { lag: i64 },
}

pub async fn run(svc: Arc<dyn LiveService>) -> Result<LiveExit, IngesterError> {
    let chain_id = svc.chain_id();
    let poll = Duration::from_millis(svc.poll_ms());
    info!(chain_id, poll_ms = svc.poll_ms(), "live mode start");

    loop {
        // Retries are per tick, so a chain that recovers does not carry a
        // failure budget forward. Exhausting them returns the error, which
        // releases the advisory lock and lets a standby try the chain.
        let outcome = retrying(Policy::LIVE_TICK, "live tick", chain_id, || svc.tick()).await?;

        if let TickOutcome::Lagging { lag } = outcome {
            info!(
                chain_id,
                lag, "lag exceeds threshold; returning to backfill"
            );
            return Ok(LiveExit::Lagging { lag });
        }
        tokio::time::sleep(poll).await;
    }
}
