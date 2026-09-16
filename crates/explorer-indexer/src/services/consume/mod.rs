//! Explorer consume service.
//!
//! Wraps the per-chain tick logic in a trait so `main` can run it through
//! `shared::tick`, mirroring fmd-indexer.
//!
//! One module per projection — `flows`, `yield_fees` — building its rows from a
//! decoded event. `events` is the routing table between the two, `plan` the
//! container and its writes, `tick` the loop and [`RefreshGate`] the
//! materialized-view gate.

mod events;
mod flows;
mod plan;
mod refresh;
mod tick;
mod yield_fees;

pub use refresh::RefreshGate;
pub use tick::{ConsumeCtx, tick_chain};

use crate::adapters::ChainLocks;
use crate::domain::error::Result;
use async_trait::async_trait;
use database::{CursorRepo, DbPool};
use shared::tick::TickProgress;
use std::sync::Arc;

#[async_trait]
pub trait ConsumeService: Send + Sync {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress>;
    async fn list_chain_ids(&self) -> Vec<i64>;
}

pub struct ConsumeServiceImpl {
    pool: DbPool,
    /// Shared by every chain: the views are global, so one refresh serves all of
    /// them. See [`RefreshGate`].
    refresh: Arc<RefreshGate>,
    /// Per-chain leadership. Shared with every tick so the lock this process
    /// holds outlives the tick that took it.
    locks: Arc<ChainLocks>,
}

impl ConsumeServiceImpl {
    pub fn new(pool: DbPool, locks: Arc<ChainLocks>) -> Self {
        Self {
            pool,
            refresh: Arc::new(RefreshGate::new()),
            locks,
        }
    }

    fn ctx(&self) -> ConsumeCtx {
        ConsumeCtx {
            pool: self.pool.clone(),
            refresh: self.refresh.clone(),
            locks: self.locks.clone(),
        }
    }
}

#[async_trait]
impl ConsumeService for ConsumeServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress> {
        tick::tick_chain(&self.ctx(), chain_id, batch).await
    }

    /// An empty list on failure: the driver then idles and retries, which is the
    /// right outcome for a transient pool error. Logged so it is not silent.
    async fn list_chain_ids(&self) -> Vec<i64> {
        database::PostgresCursorRepo::new(self.pool.clone())
            .list_chain_ids()
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "listing chain ids failed");
                Vec::new()
            })
    }
}

#[async_trait]
impl shared::tick::TickService for ConsumeServiceImpl {
    fn name(&self) -> &'static str {
        tick::NAME
    }
    async fn list_chain_ids(&self) -> Vec<i64> {
        ConsumeService::list_chain_ids(self).await
    }
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<TickProgress> {
        ConsumeService::tick_chain(self, chain_id, batch)
            .await
            .map_err(Into::into)
    }
}
