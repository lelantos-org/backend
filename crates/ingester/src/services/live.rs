//! Live-tail service.
//!
//! Owns one tick of the live ingestion loop: fetch tip, fetch logs since
//! cursor, detect reorgs, commit. The handler layer's job is just to call
//! `tick()` on a schedule and respect shutdown.

use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::domain::error::IngesterError;
use crate::domain::models::TickOutcome;
use crate::repositories::ChainStateRepo;
use crate::services::decode::logs_to_rows;
use crate::services::ingest::IngestService;
use crate::services::reorg::ReorgService;
use alloy::primitives::Address;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, info, warn};

#[async_trait]
pub trait LiveService: Send + Sync {
    async fn tick(&self) -> Result<TickOutcome, IngesterError>;
    fn poll_ms(&self) -> u64;
    fn chain_id(&self) -> i64;
}

pub struct LiveServiceImpl {
    pub cfg: ChainConfig,
    pub pool_addr: Address,
    pub rpc: DynRpc,
    pub chain_state: Arc<dyn ChainStateRepo>,
    pub ingest: Arc<IngestService>,
    pub reorg: Arc<ReorgService>,
}

#[async_trait]
impl LiveService for LiveServiceImpl {
    fn chain_id(&self) -> i64 {
        self.cfg.chain_id
    }
    fn poll_ms(&self) -> u64 {
        self.cfg.block_poll_ms
    }

    async fn tick(&self) -> Result<TickOutcome, IngesterError> {
        let chain_id = self.cfg.chain_id;
        let cursor = self.chain_state.fetch(chain_id).await?;
        let last_scanned = cursor
            .as_ref()
            .map(|s| s.last_scanned_block)
            .unwrap_or(self.cfg.start_block - 1);
        let tip_n = self.rpc.tip().await? as i64;
        let from = last_scanned + 1;
        let to = tip_n;
        if from > to {
            return Ok(TickOutcome::Idle);
        }

        let logs = self
            .rpc
            .fetch_logs(self.pool_addr, from as u64, to as u64)
            .await?;
        if logs.is_empty() {
            self.ingest.advance_empty(chain_id, to).await?;
            return Ok(TickOutcome::Empty { to });
        }

        let block_numbers: Vec<u64> = logs
            .iter()
            .filter_map(|l| l.block_number)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let block_meta = self.rpc.fetch_block_meta(&block_numbers).await?;
        let rows = logs_to_rows(chain_id, logs, &block_meta);

        if let Some(rewind_to) = self.reorg.detect(chain_id, &rows).await? {
            warn!(chain_id, rewind_to, "reorg detected");
            self.reorg.rewind(chain_id, rewind_to).await?;
            return Ok(TickOutcome::Reorg { rewind_to });
        }

        self.ingest.commit_batch(chain_id, &rows, to).await?;
        let inserted = rows.len();
        debug!(chain_id, from, to, inserted, "live commit");
        if inserted > 0 {
            info!(chain_id, from, to, inserted, "live events committed");
        }
        Ok(TickOutcome::Committed {
            count: rows.len(),
            to,
        })
    }
}
