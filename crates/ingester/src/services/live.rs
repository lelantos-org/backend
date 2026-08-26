//! Live-tail service.
//!
//! Owns one tick: verify the reorg anchor, then scan forward from the cursor and
//! commit. The handler layer calls `tick()` on a schedule.
//!
//! # Head buffer
//!
//! The scan runs all the way to `tip` rather than stopping at
//! `tip - reorg_depth`, which bounds the backfill's safe range and the depth of
//! the anchor walk rather than the live head. Head latency is therefore zero, at
//! the cost of ingesting blocks a reorg can still remove, which is why
//! [`ReorgService::check_anchor`] runs first on every tick and a rewind emits a
//! retraction signal for consumers.

use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::domain::error::IngesterError;
use crate::domain::models::TickOutcome;
use crate::repositories::ChainStateRepo;
use crate::services::decode::{distinct_blocks, logs_to_rows};
use crate::services::ingest::IngestService;
use crate::services::log_range::fetch_adaptive;
use crate::services::reorg::ReorgService;
use alloy::primitives::Address;
use async_trait::async_trait;
use shared::metrics::{record_event_age, stage};
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

/// What the next tick should scan, once the cursor is known to be sound.
enum Plan {
    /// Cursor is at tip.
    UpToDate,
    /// Too far behind for a tail, so control returns to backfill.
    Lagging {
        lag: i64,
    },
    Scan {
        from: i64,
        to: i64,
    },
}

impl LiveServiceImpl {
    /// Re-verify the anchor and rewind if the chain has moved under us.
    ///
    /// Returns `Some` when a rewind happened, in which case the tick stops: the
    /// cursor has changed and the next tick re-derives from it.
    async fn settle_reorg(&self) -> Result<Option<TickOutcome>, IngesterError> {
        let chain_id = self.cfg.chain_id;
        let Some(divergence) = self
            .reorg
            .check_anchor(
                chain_id,
                &self.rpc,
                self.cfg.start_block,
                self.cfg.reorg_depth,
            )
            .await?
        else {
            return Ok(None);
        };
        warn!(chain_id, rewind_to = divergence.rewind_to, "reorg detected");
        self.reorg.rewind(chain_id, &divergence).await?;
        Ok(Some(TickOutcome::Reorg {
            rewind_to: divergence.rewind_to,
        }))
    }

    async fn plan(&self) -> Result<Plan, IngesterError> {
        let cursor = self.chain_state.fetch(self.cfg.chain_id).await?;
        let last_scanned = cursor
            .as_ref()
            .map(|c| c.last_scanned_block)
            .unwrap_or(self.cfg.start_block - 1);
        let tip = self.rpc.tip().await? as i64;
        let from = last_scanned + 1;
        if from > tip {
            return Ok(Plan::UpToDate);
        }
        // Far enough behind that chunked, parallel backfill applies. Handing
        // control back lets the worker re-enter it rather than closing the gap one
        // poll interval at a time.
        let lag = tip - last_scanned;
        if lag > self.cfg.backfill_threshold as i64 {
            return Ok(Plan::Lagging { lag });
        }
        // Cap the span even inside the threshold: after a stall the gap can still
        // exceed what a provider serves in one `eth_getLogs`.
        let to = tip.min(from + self.cfg.chunk_blocks as i64 - 1);
        Ok(Plan::Scan { from, to })
    }

    async fn scan(&self, from: i64, to: i64) -> Result<TickOutcome, IngesterError> {
        let chain_id = self.cfg.chain_id;
        // The same adaptive fetcher the backfill uses, so a provider-side range
        // cap narrows the window rather than failing the tick.
        let logs = fetch_adaptive(&self.rpc, self.pool_addr, from as u64, to as u64).await?;
        if logs.is_empty() {
            self.ingest.advance_empty(chain_id, to).await?;
            return Ok(TickOutcome::Empty { to });
        }

        let block_meta = self.rpc.fetch_block_meta(&distinct_blocks(&logs)).await?;
        let rows = logs_to_rows(chain_id, logs, &block_meta)?;
        let inserted = self.ingest.commit_batch(chain_id, &rows, to).await?;

        // Live path only. `commit_batch` also serves the backfill, where the age
        // is that of history rather than of the head, and mixing the two would
        // make the histogram unreadable.
        if let Some(newest) = rows.iter().map(|r| r.block_ts).max() {
            record_event_age(stage::INGEST, chain_id, newest);
        }

        debug!(chain_id, from, to, inserted, "live commit");
        if inserted > 0 {
            info!(chain_id, from, to, inserted, "live events committed");
        }
        Ok(TickOutcome::Committed {
            count: inserted,
            to,
        })
    }
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
        // Reorg check first. The cursor is only meaningful while the block it
        // anchors to is canonical, so scanning forward from an unverified cursor
        // would extend an abandoned branch.
        if let Some(outcome) = self.settle_reorg().await? {
            return Ok(outcome);
        }
        match self.plan().await? {
            Plan::UpToDate => Ok(TickOutcome::Idle),
            Plan::Lagging { lag } => Ok(TickOutcome::Lagging { lag }),
            Plan::Scan { from, to } => self.scan(from, to).await,
        }
    }
}
