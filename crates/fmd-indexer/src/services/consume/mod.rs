//! Drain ingested clue events into the FMD pipeline.
//!
//! One tick reads a window of `raw_events`, decodes it into a [`CommitPlan`]
//! ([`crate::domain::pending`]), writes that plan one batched call per table,
//! and only then advances the cursor. Frontier folding lives in [`tree`], the
//! escrow side lookup in [`crate::domain::escrow`]; what is left here is the
//! orchestration.
//!
//! Serialised per chain by a Postgres advisory lock. Every write below is a
//! read-then-write with no transaction — the cursor and the `spent_nullifiers`
//! ordinal assignment — and corrupts under a second writer.

mod tree;

use crate::adapters::locks::ChainLocks;
use crate::domain::error::Result;
use crate::domain::escrow::{EscrowedMap, decode_escrowed, flushed_deposit_ids};
use crate::domain::pending::plan_commit;
use crate::repositories::cursor::{CursorRepo, UpsertCursor};
use crate::repositories::notes::NotesRepo;
use crate::repositories::raw_events::{RawEventRow, RawEventsRepo};
use crate::repositories::spent_nullifiers::SpentNullifiersRepo;
use crate::repositories::tree_state::TreeStateRepo;
use async_trait::async_trait;
use shared::entities::EventKind;
use shared::metrics::{record_event_age, stage};
use shared::tick::TickProgress;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// The plan one tick commits. Defined in the planning domain; re-exported here
/// because this is the batched-write reference `ARCHITECTURE.md` names.
pub use crate::domain::pending::CommitPlan;

pub const NAME: &str = "fmd";

/// Whether this service consumes a kind.
///
/// The predicate, not a list — `kinds()` below is derived from it, so the
/// `WHERE event_kind = ANY` of the fetch cannot fall behind the decision. No
/// wildcard arm, so a new `EventKind` variant fails to compile here and has to
/// be classified deliberately.
///
/// explorer-indexer had the mirror-image bug: its filter was a hand-written
/// array, the yield kinds were added to the enum but not to it, and their
/// handlers were silently unreachable for as long as the mixin had been live.
/// This service reads the FMD zone plus the two kinds it needs for ordering.
const fn consumed(kind: EventKind) -> bool {
    match kind {
        EventKind::NoteCreated
        | EventKind::RootAdvanced
        | EventKind::NullifierConsumed
        | EventKind::DepositFlushed => true,
        EventKind::AssetRegistered
        | EventKind::AssetMoved
        | EventKind::DepositEscrowed
        | EventKind::DepositCanceled
        | EventKind::AssetFeeSet
        | EventKind::YieldAssetAdded
        | EventKind::YieldParamsSet
        | EventKind::PerfFeeAccrued
        | EventKind::NormalizedFeeSwept
        | EventKind::Rebalanced
        | EventKind::HaltedSet
        | EventKind::EmergencyUnwound => false,
    }
}

/// The kinds this service fetches, as the `ANY` array wants them.
fn kinds() -> &'static [i16] {
    static KINDS: OnceLock<Vec<i16>> = OnceLock::new();
    KINDS.get_or_init(|| {
        EventKind::ALL
            .into_iter()
            .filter(|k| consumed(*k))
            .map(EventKind::as_i16)
            .collect()
    })
}

/// How far the window may be widened when a saturated one is entirely occupied by
/// a transaction that cannot fit. A transaction needing more than 16 times
/// `batch` rows is not a batch-sizing problem, so widening stops and the stall
/// alarm fires.
const MAX_WINDOW_GROWTH: i64 = 16;

/// Consecutive no-progress ticks before deferral is treated as a stall rather
/// than a normal wait for the next block. One minute at the default tick.
const STALL_TICKS: u32 = 120;
/// Re-report cadence once stalled, so a wedged chain stays visible without
/// filling the log at tick rate.
const STALL_REPEAT_TICKS: u32 = STALL_TICKS * 10;

#[async_trait]
pub trait ConsumeService: Send + Sync {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress>;
    async fn list_chain_ids(&self) -> Vec<i64>;
}

/// What one window of raw events yielded.
enum Planned {
    /// Nothing queued past the cursor.
    Drained,
    /// Rows are queued, but the transaction at the head is not fully observed.
    Incomplete { rows: usize },
    Ready {
        plan: CommitPlan,
        /// The window came back full, so more rows are queued behind this commit.
        /// Drives [`TickProgress::Saturated`].
        window_full: bool,
        /// `block_ts` of the newest row this plan commits, for the event-age
        /// histogram. Carried here rather than in [`CommitPlan`] to keep the
        /// planning domain free of instrumentation.
        last_block_ts: Option<i64>,
    },
}

pub struct ConsumeServiceImpl {
    /// Used directly for reorg retraction, which spans several derived tables and
    /// so has no single repository behind it.
    pool: database::DbPool,
    cursors: Arc<dyn CursorRepo>,
    raw_events: Arc<dyn RawEventsRepo>,
    notes: Arc<dyn NotesRepo>,
    spent_nfs: Arc<dyn SpentNullifiersRepo>,
    tree_state: Arc<dyn TreeStateRepo>,
    locks: ChainLocks,
    stalls: StallTracker,
}

impl ConsumeServiceImpl {
    pub fn new(
        pool: database::DbPool,
        cursors: Arc<dyn CursorRepo>,
        raw_events: Arc<dyn RawEventsRepo>,
        notes: Arc<dyn NotesRepo>,
        spent_nfs: Arc<dyn SpentNullifiersRepo>,
        tree_state: Arc<dyn TreeStateRepo>,
        locks: ChainLocks,
    ) -> Self {
        Self {
            pool,
            cursors,
            raw_events,
            notes,
            spent_nfs,
            tree_state,
            locks,
            stalls: StallTracker::default(),
        }
    }

    /// Plan the next commit, widening the window while a saturated one keeps
    /// yielding nothing.
    ///
    /// A transaction is committable only once all its events sit in one window,
    /// and re-ticking fetches the same rows, so a transaction wider than `batch`
    /// would otherwise never commit.
    async fn plan_next(&self, chain_id: i64, after: i64, batch: i64) -> Result<Planned> {
        let mut limit = batch;
        loop {
            let rows = self
                .raw_events
                .batch_after(chain_id, after, kinds(), limit)
                .await?;
            if rows.is_empty() {
                return Ok(Planned::Drained);
            }

            let escrowed = self.resolve_escrowed(chain_id, &rows).await?;
            let saturated = rows.len() as i64 == limit;
            if let Some(plan) = plan_commit(&rows, chain_id, after, &escrowed)? {
                let last_block_ts = Self::committed_block_ts(&rows, &plan);
                return Ok(Planned::Ready {
                    plan,
                    window_full: saturated,
                    last_block_ts,
                });
            }

            if !saturated || limit >= batch * MAX_WINDOW_GROWTH {
                return Ok(Planned::Incomplete { rows: rows.len() });
            }
            limit *= 2;
            warn!(
                chain_id,
                limit, "window saturated by an incomplete tx; widening and retrying"
            );
        }
    }

    /// `block_ts` of the newest row `plan` actually commits.
    ///
    /// The plan stops at a transaction boundary inside the window, so this is the
    /// row it ends on rather than the newest row read.
    ///
    /// `None` means `plan.last_event_id` was absent from the window it was built
    /// from, which is a bug. The sample is omitted rather than defaulted: a `0`
    /// is a 1970 timestamp, which the histogram would record as a ~56-year event
    /// age and distort every percentile.
    fn committed_block_ts(rows: &[RawEventRow], plan: &CommitPlan) -> Option<i64> {
        rows.iter()
            .find(|r| r.id == plan.last_event_id)
            .map(|r| r.block_ts)
    }

    /// Look up the `DepositEscrowed` payloads the window's `DepositFlushed`
    /// events refer to, keyed by deposit id.
    async fn resolve_escrowed(&self, chain_id: i64, rows: &[RawEventRow]) -> Result<EscrowedMap> {
        let deposit_ids = flushed_deposit_ids(rows);
        if deposit_ids.is_empty() {
            return Ok(EscrowedMap::new());
        }

        let escrowed = self
            .raw_events
            .fetch_escrowed_by_ids(chain_id, &deposit_ids)
            .await?;

        let mut out = EscrowedMap::with_capacity(escrowed.len());
        for row in &escrowed {
            match decode_escrowed(row) {
                // `fetch_escrowed_by_ids` orders by id, so a re-used deposit id
                // resolves to its earliest escrow, identically on every replica.
                Some((id, payload)) => {
                    out.entry(id).or_insert(payload);
                }
                None => warn!(chain_id, "decode DepositEscrowed failed; skipping"),
            }
        }
        Ok(out)
    }

    async fn commit(&self, chain_id: i64, plan: CommitPlan) -> Result<()> {
        self.notes.insert_batch(&plan.notes).await?;
        self.spent_nfs.insert_batch(&plan.spent_nfs).await?;
        // After the notes, never before: a crash in between leaves the tree
        // behind, which the guard in `tree::advance` repairs on the next tick.
        // The other order would publish a root committing to notes no client can
        // fetch yet.
        tree::advance(
            self.notes.as_ref(),
            self.tree_state.as_ref(),
            chain_id,
            &plan.leaves,
        )
        .await?;
        // Monotonic, so a peer that is ahead is never dragged backwards. The
        // reset in `tick_chain` uses plain `upsert` because rewinding is its
        // purpose.
        let advanced = self
            .cursors
            .upsert_monotonic(UpsertCursor {
                name: NAME.to_string(),
                chain_id,
                last_event_id: plan.last_event_id,
                last_block_number: plan.last_block_number,
            })
            .await?;
        if !advanced {
            // This tick holds the chain's advisory lock, so nothing else should
            // move this cursor. A rejected advance means a second writer holds
            // the lock; the rows above were written, so the cost is duplicate
            // work rather than loss, but it must be reported.
            error!(
                chain_id,
                last_event_id = plan.last_event_id,
                "cursor advance rejected while holding the chain lock; \
                 a second writer is active for this chain"
            );
        }

        if let Some(max_leaf) = plan.notes.iter().map(|n| n.leaf_index).max() {
            metrics::gauge!(
                shared::metrics::name::NOTES_LEAF_INDEX_MAX,
                "chain_id" => chain_id.to_string(),
            )
            .set(max_leaf as f64);
        }

        self.notes.notify_appended(chain_id).await;

        let (notes, spent_nfs) = (plan.notes.len(), plan.spent_nfs.len());
        debug!(
            chain_id,
            notes,
            spent_nfs,
            last_id = plan.last_event_id,
            last_block = plan.last_block_number,
            "consume commit"
        );
        if notes > 0 || spent_nfs > 0 {
            info!(
                chain_id,
                notes,
                spent_nfs,
                last_block = plan.last_block_number,
                "consume committed events"
            );
        }
        Ok(())
    }

    /// Rewind a cursor pointing past the end of `raw_events`, which happens when
    /// the table has been truncated or re-ingested. Without the reset every read
    /// comes back empty.
    async fn reset_cursor(&self, chain_id: i64) -> Result<()> {
        warn!(chain_id, "cursor ahead of raw_events.max_id; reset to 0");
        self.cursors
            .upsert(UpsertCursor {
                name: NAME.to_string(),
                chain_id,
                last_event_id: 0,
                last_block_number: 0,
            })
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ConsumeService for ConsumeServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<TickProgress> {
        // A standby replica has nothing to do; idling lets the backoff grow
        // rather than polling the lock at full speed.
        let leader = self.locks.is_leader(chain_id).await?;
        shared::metrics::record_chain_leader(chain_id, leader);
        if !leader {
            return Ok(TickProgress::Idle);
        }

        // Retract before reading. Replacement rows for a reorged range come back
        // with fresh, higher ids and replay on their own, but the notes and
        // nullifiers derived from the deleted rows sit below the cursor where
        // nothing revisits them, leaving the tree describing an abandoned branch.
        // Applying the reorg log first drops those and rewinds the cursor so the
        // replay rebuilds them, which is queued work rather than a reason to
        // sleep.
        let reorgs =
            database::reorg::apply_pending(&self.pool, database::reorg::Owner::Fmd, chain_id)
                .await?;
        if reorgs > 0 {
            metrics::counter!(
                shared::metrics::name::REORGS_APPLIED,
                "chain_id" => chain_id.to_string(),
            )
            .increment(reorgs as u64);
            return Ok(TickProgress::Saturated);
        }

        let (after, _last_block) = self.cursors.fetch(NAME, chain_id).await?;
        let max_id = self.raw_events.max_id(chain_id).await?;
        // The pair is the lag signal: `max_id - cursor` is how far behind this
        // consumer is. Emitted as two gauges rather than a difference so a
        // stalled ingester and a stalled consumer stay distinguishable.
        let chain = chain_id.to_string();
        metrics::gauge!(
            shared::metrics::name::CONSUMER_CURSOR_EVENT_ID,
            "service" => NAME,
            "chain_id" => chain.clone(),
        )
        .set(after as f64);
        metrics::gauge!(
            shared::metrics::name::RAW_EVENTS_MAX_ID,
            "service" => NAME,
            "chain_id" => chain,
        )
        .set(max_id as f64);
        if after > max_id {
            self.reset_cursor(chain_id).await?;
            return Ok(TickProgress::Saturated);
        }

        match self.plan_next(chain_id, after, batch).await? {
            Planned::Drained => Ok(TickProgress::Idle),
            // The cursor did not move. Reporting progress here would spin the
            // driver at zero delay against a transaction it cannot yet commit.
            Planned::Incomplete { rows } => {
                self.stalls.record_idle(chain_id, after, rows).await;
                Ok(TickProgress::Idle)
            }
            Planned::Ready {
                plan,
                window_full,
                last_block_ts,
            } => {
                self.commit(chain_id, plan).await?;
                self.stalls.clear(chain_id).await;
                if let Some(block_ts) = last_block_ts {
                    record_event_age(stage::CONSUME, chain_id, block_ts);
                }
                Ok(TickProgress::advanced(window_full))
            }
        }
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        super::chain_ids(self.cursors.as_ref()).await
    }
}

#[async_trait]
impl shared::tick::TickService for ConsumeServiceImpl {
    fn name(&self) -> &'static str {
        NAME
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

/// How long each chain has been parked on the same cursor.
///
/// Deferring a transaction is normal for one tick and an outage after a thousand.
/// The tick returns `Ok(())` either way, so this counter distinguishes them.
#[derive(Default)]
struct StallTracker(Mutex<HashMap<i64, Stall>>);

struct Stall {
    cursor: i64,
    ticks: u32,
}

impl StallTracker {
    async fn record_idle(&self, chain_id: i64, cursor: i64, rows: usize) {
        let mut stalls = self.0.lock().await;
        let stall = stalls.entry(chain_id).or_insert(Stall { cursor, ticks: 0 });
        if stall.cursor != cursor {
            *stall = Stall { cursor, ticks: 0 };
        }
        stall.ticks += 1;

        let overdue = stall.ticks.checked_sub(STALL_TICKS);
        if overdue.is_some_and(|n| n % STALL_REPEAT_TICKS == 0) {
            error!(
                chain_id,
                cursor,
                rows,
                ticks = stall.ticks,
                "consume has committed nothing for {} consecutive ticks; the head tx cannot be \
                 completed (missing DepositEscrowed, or a tx wider than the batch window)",
                stall.ticks
            );
        }
    }

    async fn clear(&self, chain_id: i64) {
        self.0.lock().await.remove(&chain_id);
    }
}
