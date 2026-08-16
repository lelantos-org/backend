//! Explorer consume service.
//!
//! Wraps the per-chain tick logic in a trait so the binary main can run it
//! through `shared::tick`, mirroring fmd-indexer.

mod events;
mod tick;

pub use tick::{ConsumeCtx, tick_chain};

use crate::adapters::DynTokenMetadata;
use crate::config::ExplorerIndexerConfig;
use crate::error::Result;
use async_trait::async_trait;
use database::DbPool;
use std::collections::HashMap;
use std::sync::Arc;

#[async_trait]
pub trait ConsumeService: Send + Sync {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<()>;
    async fn list_chain_ids(&self) -> Vec<i64>;
}

pub struct ConsumeServiceImpl {
    pub pool: DbPool,
    pub cfg: Arc<ExplorerIndexerConfig>,
    pub token_meta: Arc<HashMap<i64, DynTokenMetadata>>,
}

impl ConsumeServiceImpl {
    pub fn new(
        pool: DbPool,
        cfg: Arc<ExplorerIndexerConfig>,
        token_meta: Arc<HashMap<i64, DynTokenMetadata>>,
    ) -> Self {
        Self {
            pool,
            cfg,
            token_meta,
        }
    }

    fn ctx(&self) -> ConsumeCtx {
        ConsumeCtx {
            pool: self.pool.clone(),
            cfg: self.cfg.clone(),
            token_meta: self.token_meta.clone(),
        }
    }
}

#[async_trait]
impl ConsumeService for ConsumeServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<()> {
        tick::tick_chain(&self.ctx(), chain_id, batch).await
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        use database::CursorRepo;
        database::PostgresCursorRepo::new(self.pool.clone())
            .list_chain_ids()
            .await
            .unwrap_or_default()
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
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<()> {
        ConsumeService::tick_chain(self, chain_id, batch)
            .await
            .map_err(Into::into)
    }
}
