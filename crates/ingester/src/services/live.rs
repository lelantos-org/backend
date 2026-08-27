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
use crate::services::ingest::IngestService;
use crate::services::log_range::{LogWindow, fetch_rows};
use crate::services::reorg::{Checkpoint, ReorgService, anchor_of};
use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use shared::metrics::{
    ingest_stage, record_chain_lag, record_event_age, stage, timed_ingest_stage,
};
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
    pub log_window: Arc<LogWindow>,
}

/// Everything one tick reads before it decides anything.
///
/// Holds what the tick uses, not what it read: the cursor is consumed into
/// `last_scanned` and `anchor` at survey time, so no two fields here can
/// disagree about it.
struct Survey {
    /// The cursor's watermark, defaulted for a chain that has committed nothing.
    last_scanned: i64,
    /// The cursor's verified anchor, absent on a chain that has committed nothing.
    anchor: Option<Checkpoint>,
    /// What the chain reports at the anchor's height. `None` when there is no
    /// anchor to check, and also when the chain no longer has that block — both
    /// mean the stored hash cannot be confirmed.
    chain_hash: Option<B256>,
    tip: i64,
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
    /// Read the cursor, then ask the chain for the anchor's hash and the tip at
    /// the same time.
    ///
    /// One DB round trip and one pair of overlapping RPC round trips, where the
    /// obvious spelling costs five in series: the anchor check and the plan both
    /// need the cursor, and neither RPC depends on the other's answer.
    /// `catch_up` already overlaps its two the same way.
    async fn survey(&self) -> Result<Survey, IngesterError> {
        let chain_id = self.cfg.chain_id;
        let cursor = timed_ingest_stage(
            ingest_stage::PLAN,
            chain_id,
            self.chain_state.fetch(chain_id),
        )
        .await?;
        let last_scanned = cursor
            .as_ref()
            .map(|c| c.last_scanned_block)
            .unwrap_or(self.cfg.start_block - 1);
        let anchor = cursor.as_ref().and_then(anchor_of);
        let anchor_block = anchor.as_ref().map(|(b, _)| *b as u64);

        // A chain with no anchor still needs the tip, so the hash lookup
        // resolves to `None` rather than becoming a second call shape.
        let (chain_hash, tip) = timed_ingest_stage(ingest_stage::ANCHOR, chain_id, async {
            tokio::try_join!(
                async {
                    match anchor_block {
                        Some(n) => self.rpc.block_hash_at(n).await,
                        None => Ok(None),
                    }
                },
                self.rpc.tip(),
            )
        })
        .await?;

        Ok(Survey {
            last_scanned,
            anchor,
            chain_hash,
            tip: tip as i64,
        })
    }

    /// Rewind if the chain has moved under the cursor.
    ///
    /// Returns `Some` when a rewind happened, in which case the tick stops: the
    /// cursor has changed and the next tick re-derives from it.
    async fn settle_reorg(&self, survey: &Survey) -> Result<Option<TickOutcome>, IngesterError> {
        let chain_id = self.cfg.chain_id;
        let Some(anchor) = survey.anchor.as_ref() else {
            return Ok(None);
        };
        let Some(divergence) = self
            .reorg
            .check_anchor(
                chain_id,
                &self.rpc,
                self.cfg.start_block,
                self.cfg.reorg_depth,
                anchor,
                survey.chain_hash,
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

    fn plan(&self, survey: &Survey) -> Plan {
        let last_scanned = survey.last_scanned;
        let tip = survey.tip;
        let from = last_scanned + 1;
        record_chain_lag(self.cfg.chain_id, tip - last_scanned);
        if from > tip {
            return Plan::UpToDate;
        }
        // Far enough behind that chunked, parallel backfill applies. Handing
        // control back lets the worker re-enter it rather than closing the gap one
        // poll interval at a time.
        let lag = tip - last_scanned;
        if lag > self.cfg.backfill_threshold as i64 {
            return Plan::Lagging { lag };
        }
        // Cap the span even inside the threshold: after a stall the gap can still
        // exceed what a provider serves in one `eth_getLogs`.
        let to = tip.min(from + self.cfg.chunk_blocks as i64 - 1);
        Plan::Scan { from, to }
    }

    async fn scan(
        &self,
        from: i64,
        to: i64,
        reached_tip: bool,
    ) -> Result<TickOutcome, IngesterError> {
        let chain_id = self.cfg.chain_id;
        // The same adaptive fetcher the backfill uses, so a provider-side range
        // cap narrows the window rather than failing the tick.
        let rows = fetch_rows(
            &self.rpc,
            &self.log_window,
            chain_id,
            self.pool_addr,
            from as u64,
            to as u64,
        )
        .await?;
        if rows.is_empty() {
            self.ingest.advance_empty(chain_id, to).await?;
            return Ok(TickOutcome::Empty { to, reached_tip });
        }
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
            reached_tip,
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
        let survey = self.survey().await?;
        // Reorg check first. The cursor is only meaningful while the block it
        // anchors to is canonical, so scanning forward from an unverified cursor
        // would extend an abandoned branch. A rewind invalidates the surveyed
        // tip's usefulness too, which is why it ends the tick.
        if let Some(outcome) = self.settle_reorg(&survey).await? {
            return Ok(outcome);
        }
        match self.plan(&survey) {
            Plan::UpToDate => Ok(TickOutcome::Idle),
            Plan::Lagging { lag } => Ok(TickOutcome::Lagging { lag }),
            Plan::Scan { from, to } => self.scan(from, to, to == survey.tip).await,
        }
    }
}
