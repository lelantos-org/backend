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
use crate::domain::models::{Checkpoint, TickOutcome, scanned_watermark};
use crate::repositories::ChainStateRepo;
use crate::services::ingest::IngestService;
use crate::services::log_range::{LogWindow, fetch_rows};
use crate::services::reorg::{ReorgService, anchor_of};
use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use shared::metrics::{
    ingest_stage, record_chain_lag, record_event_age, stage, timed_ingest_stage,
};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tracing::{debug, info, warn};

#[async_trait]
pub trait LiveService: Send + Sync {
    async fn tick(&self) -> Result<TickOutcome, IngesterError>;
    /// Drop anything remembered about the cursor, forcing the next tick to read
    /// it from Postgres.
    ///
    /// Called on entry to live mode, because the backfill writes the same row and
    /// this service is reused across the alternation between the two.
    fn forget_cursor(&self);
    fn poll_ms(&self) -> u64;
    fn chain_id(&self) -> i64;
}

/// What the previous tick left the cursor at.
///
/// Only the two things [`Survey`] takes from it, so nothing here can outlive its
/// usefulness or disagree with the row it mirrors.
#[derive(Debug, Clone)]
struct CachedCursor {
    last_scanned: i64,
    anchor: Option<Checkpoint>,
}

/// Fields are private because one of them is an invariant rather than a setting:
/// see `cursor`.
pub struct LiveServiceImpl {
    cfg: ChainConfig,
    pool_addr: Address,
    rpc: DynRpc,
    chain_state: Arc<dyn ChainStateRepo>,
    ingest: Arc<IngestService>,
    reorg: Arc<ReorgService>,
    log_window: Arc<LogWindow>,
    /// The cursor as this service last left it, so a steady tick costs one round
    /// trip instead of two.
    ///
    /// Sound only because the writer is singular: the advisory lock keeps one
    /// process on a chain, and within it the backfill and the live tail
    /// alternate rather than overlap. The two ways the row can move out from
    /// under this are both handled by clearing it — a rewind, and the backfill,
    /// which [`LiveService::forget_cursor`] covers on the way back into live mode.
    ///
    /// `None` means "ask Postgres", which is also the state every failure leaves
    /// it in: nothing is recorded here that Postgres has not already accepted.
    cursor: Mutex<Option<CachedCursor>>,
}

impl LiveServiceImpl {
    pub fn new(
        cfg: ChainConfig,
        pool_addr: Address,
        rpc: DynRpc,
        chain_state: Arc<dyn ChainStateRepo>,
        ingest: Arc<IngestService>,
        reorg: Arc<ReorgService>,
        log_window: Arc<LogWindow>,
    ) -> Self {
        Self {
            cfg,
            pool_addr,
            rpc,
            chain_state,
            ingest,
            reorg,
            log_window,
            cursor: Mutex::new(None),
        }
    }

    /// The cache, recovering from a poisoned lock rather than propagating it.
    ///
    /// Poisoning here would mean a tick panicked mid-write, but what it guards is
    /// replaced wholesale and never mutated in place, so there is no torn state
    /// to protect anyone from — and refusing the lock would wedge the chain for
    /// good over a fault the next tick would otherwise re-read past.
    fn cache(&self) -> MutexGuard<'_, Option<CachedCursor>> {
        self.cursor.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replace what the next tick will assume about the cursor.
    ///
    /// Only ever called with a value Postgres has already accepted.
    fn remember(&self, cursor: CachedCursor) {
        *self.cache() = Some(cursor);
    }

    fn forget(&self) {
        *self.cache() = None;
    }
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
    /// Where the cursor stands, from memory when this service is what last moved
    /// it and from Postgres otherwise.
    ///
    /// The read is skippable rather than merely overlappable: the anchor lookup
    /// needs the cursor, so the two cannot be joined, and a tail that is caught
    /// up would otherwise spend a round trip every poll being told what it
    /// already knows.
    async fn cursor(&self) -> Result<CachedCursor, IngesterError> {
        if let Some(cached) = self.cache().clone() {
            return Ok(cached);
        }
        let chain_id = self.cfg.chain_id;
        let row = timed_ingest_stage(
            ingest_stage::PLAN,
            chain_id,
            self.chain_state.fetch(chain_id),
        )
        .await?;
        let cursor = CachedCursor {
            last_scanned: scanned_watermark(row.as_ref(), self.cfg.start_block),
            anchor: row.as_ref().and_then(anchor_of),
        };
        self.remember(cursor.clone());
        Ok(cursor)
    }

    /// Read the cursor, then ask the chain for the anchor's hash and the tip at
    /// the same time.
    ///
    /// The two RPCs overlap because neither depends on the other's answer; the
    /// obvious spelling costs five round trips in series. `catch_up` already
    /// overlaps its two the same way.
    async fn survey(&self) -> Result<Survey, IngesterError> {
        let chain_id = self.cfg.chain_id;
        let CachedCursor {
            last_scanned,
            anchor,
        } = self.cursor().await?;
        let anchor_block = anchor.as_ref().map(|a| a.block as u64);

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
        // Cleared rather than recomputed. A rewind rewrites the anchor from
        // whatever survived the fork, and re-deriving that here would be a second
        // copy of `ReorgService::rewind`'s rule; the next tick pays one read
        // instead, which a reorg is rare enough to afford.
        self.forget();
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

    /// Fetch `[from, to]`, commit whatever it holds, and record where that left
    /// the cursor.
    ///
    /// Takes the whole survey because the outcome describes it: whether the scan
    /// reached the tip, and which anchor survives an empty range, are both facts
    /// about what was surveyed rather than about the range.
    async fn scan(
        &self,
        survey: &Survey,
        from: i64,
        to: i64,
    ) -> Result<TickOutcome, IngesterError> {
        let chain_id = self.cfg.chain_id;
        let reached_tip = to == survey.tip;
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
            // An empty range moves the watermark and nothing else: it holds no
            // block whose hash was seen, so the surveyed anchor still stands.
            self.remember(CachedCursor {
                last_scanned: to,
                anchor: survey.anchor.clone(),
            });
            return Ok(TickOutcome::Empty { to, reached_tip });
        }
        let committed = IngestService::cursor_for(chain_id, &rows, to);
        let inserted = self.ingest.commit_batch(chain_id, &rows, to).await?;
        // After the commit, so nothing is remembered that Postgres rejected. The
        // anchor comes from `cursor_for` rather than being rebuilt here, so what
        // is remembered and what was written cannot disagree.
        self.remember(CachedCursor {
            last_scanned: to,
            anchor: committed.as_ref().and_then(anchor_of),
        });

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
    fn forget_cursor(&self) {
        self.forget();
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
            Plan::Scan { from, to } => self.scan(&survey, from, to).await,
        }
    }
}
