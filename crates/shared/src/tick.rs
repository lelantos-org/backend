//! Reusable tick driver for indexer-style services.
//!
//! Every indexer service repeats: enumerate chains, call `tick_chain` on
//! each, sleep, exit on shutdown. Implementing `TickService` lets a service
//! reuse the loop via [`run`] without rewriting the boilerplate.

use crate::shutdown::Shutdown;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

#[async_trait]
pub trait TickService: Send + Sync {
    /// Human-readable name for logs.
    fn name(&self) -> &'static str;
    /// Chains the service should tick on this round.
    async fn list_chain_ids(&self) -> Vec<i64>;
    /// Single tick for one chain. Errors are logged + swallowed by the
    /// driver so one chain failing doesn't stall the others.
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<()>;
}

/// Drive a `TickService` until shutdown fires.
pub async fn run(svc: Arc<dyn TickService>, tick_ms: u64, batch: i64, mut shutdown: Shutdown) {
    let name = svc.name();
    info!(name, tick_ms, batch, "tick driver started");
    loop {
        for chain_id in svc.list_chain_ids().await {
            if let Err(e) = svc.tick_chain(chain_id, batch).await {
                warn!(name, chain_id, "tick error: {}", e);
            }
        }
        tokio::select! {
            _ = sleep(Duration::from_millis(tick_ms)) => {},
            _ = shutdown.recv() => {
                info!(name, "tick driver stopping");
                return;
            }
        }
    }
}
