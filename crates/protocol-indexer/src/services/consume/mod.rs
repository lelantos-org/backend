//! Protocol consume service.
//!
//! Wraps the per-chain tick logic in a trait so `main` can run it through
//! `shared::tick`, mirroring fmd-indexer.
//!
//! One module per concern this crate projects — `assets`, `yields`, `tree`,
//! `deposits` — each owning its slice of the tick's plan and the writes that
//! land it. `events` is the routing table between the decoded event and those
//! modules, `plan` the container and the causal order, `tick` the loop,
//! `metadata` the RPC sweep beside it and [`RefreshGate`] the materialized-view
//! gate.

mod assets;
mod deposits;
mod events;
mod metadata;
mod plan;
mod refresh;
mod tick;
mod tree;
mod yields;

pub use refresh::RefreshGate;
pub use tick::{ConsumeCtx, tick_chain};

use crate::adapters::DynTokenMetadata;
use crate::app::config::ProtocolIndexerConfig;
use crate::domain::error::Result;
use async_trait::async_trait;
use database::DbPool;
use shared::tick::TickProgress;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

#[async_trait]
pub trait ConsumeService: Send + Sync {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress>;
    async fn list_chain_ids(&self) -> Vec<i64>;
}

pub struct ConsumeServiceImpl {
    pub pool: DbPool,
    pub cfg: Arc<ProtocolIndexerConfig>,
    pub token_meta: Arc<HashMap<i64, DynTokenMetadata>>,
    /// Shared by every chain: the views are global, so one refresh serves all of
    /// them. See [`RefreshGate`].
    refresh: Arc<RefreshGate>,
}

impl ConsumeServiceImpl {
    pub fn new(
        pool: DbPool,
        cfg: Arc<ProtocolIndexerConfig>,
        token_meta: Arc<HashMap<i64, DynTokenMetadata>>,
    ) -> Self {
        Self {
            pool,
            cfg,
            token_meta,
            refresh: Arc::new(RefreshGate::new()),
        }
    }

    fn ctx(&self) -> ConsumeCtx {
        ConsumeCtx {
            pool: self.pool.clone(),
            cfg: self.cfg.clone(),
            token_meta: self.token_meta.clone(),
            refresh: self.refresh.clone(),
        }
    }
}

#[async_trait]
impl ConsumeService for ConsumeServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress> {
        tick::tick_chain(&self.ctx(), chain_id, batch).await
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        use database::CursorRepo;
        match database::PostgresCursorRepo::new(self.pool.clone())
            .list_chain_ids()
            .await
        {
            Ok(ids) => ids,
            // An empty list is indistinguishable from no chains being
            // configured, so a failed read is logged rather than idled through.
            Err(e) => {
                warn!(error = %e, "list_chain_ids failed; skipping this round");
                Vec::new()
            }
        }
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
