//! The flush worker: one chain's `FlushPipeline::tick` on a fixed interval.

use crate::services::pipeline::FlushPipeline;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// Drive one chain's flush pipeline every `interval`.
///
/// Ticks are skipped rather than queued when one runs long. A failed tick,
/// a parked mirror included, only waits for the next: the batcher resyncs a
/// parked mirror when it next reserves a bundle.
pub fn spawn(flush: Arc<FlushPipeline>, interval: Duration) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = flush.tick().await {
                warn!(chain_id = flush.chain_id, error = %e, "flush tick failed");
            }
        }
    });
}
