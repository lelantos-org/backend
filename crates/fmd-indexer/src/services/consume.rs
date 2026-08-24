//! Drain ingested clue events into the FMD pipeline.
//!
//! Serialised per chain by a Postgres advisory lock: every write below is a
//! read-then-write with no transaction — the cursor, and `spent_nullifiers`
//! ordinal assignment — and silently corrupts under a second writer.

use crate::adapters::locks::ChainLocks;
use crate::domain::error::Result;
use crate::domain::pending::{CommitPlan, EscrowedMap, LeafPayload, plan_commit};
use crate::repositories::cursor::{CursorRepo, UpsertCursor};
use crate::repositories::notes::NotesRepo;
use crate::repositories::raw_events::{RawEventRow, RawEventsRepo};
use crate::repositories::spent_nullifiers::SpentNullifiersRepo;
use alloy::primitives::{B256, LogData, U256};
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use chain_types::abi::DepositEscrowed;
use shared::entities::EventKind;
use shared::tick::TickProgress;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

pub const NAME: &str = "fmd";

const KINDS: [i16; 4] = [
    EventKind::NoteCreated as i16,
    EventKind::RootAdvanced as i16,
    EventKind::NullifierConsumed as i16,
    EventKind::DepositFlushed as i16,
];

/// How far the window may be widened when a saturated one is entirely
/// occupied by a tx that cannot fit in it. A tx needing more than 16×`batch`
/// rows is not a batch-sizing problem, so stop and let the stall alarm fire.
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
    /// Rows are queued, but the tx at the head is not fully observed.
    Incomplete { rows: usize },
    Ready {
        plan: CommitPlan,
        /// The window came back full, so more rows are queued behind this
        /// commit. Drives [`TickProgress::Saturated`].
        window_full: bool,
    },
}

pub struct ConsumeServiceImpl {
    /// Needed directly for reorg retraction, which spans several derived
    /// tables and so has no single repository to sit behind.
    pool: database::DbPool,
    cursors: Arc<dyn CursorRepo>,
    raw_events: Arc<dyn RawEventsRepo>,
    notes: Arc<dyn NotesRepo>,
    spent_nfs: Arc<dyn SpentNullifiersRepo>,
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
        locks: ChainLocks,
    ) -> Self {
        Self {
            pool,
            cursors,
            raw_events,
            notes,
            spent_nfs,
            locks,
            stalls: StallTracker::default(),
        }
    }

    /// Plan the next commit, widening the window while a saturated one keeps
    /// yielding nothing.
    ///
    /// A tx is committable only once all of its events sit in one window, and
    /// re-ticking fetches the same rows — so a tx wider than `batch` would
    /// otherwise never commit, and never say why.
    async fn plan_next(&self, chain_id: i64, after: i64, batch: i64) -> Result<Planned> {
        let mut limit = batch;
        loop {
            let rows = self
                .raw_events
                .batch_after(chain_id, after, &KINDS, limit)
                .await?;
            if rows.is_empty() {
                return Ok(Planned::Drained);
            }

            let escrowed = self.resolve_escrowed(chain_id, &rows).await?;
            let saturated = rows.len() as i64 == limit;
            if let Some(plan) = plan_commit(&rows, chain_id, after, &escrowed)? {
                return Ok(Planned::Ready {
                    plan,
                    window_full: saturated,
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

    /// Look up the `DepositEscrowed` payloads the window's `DepositFlushed`
    /// events refer to, keyed by deposit id (decimal).
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
                // `fetch_escrowed_by_ids` orders by id, so a re-used deposit
                // id resolves to its earliest escrow — deterministically, and
                // identically on every replica.
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
        // Monotonic: never drag the cursor backwards if a peer is ahead. The
        // reset in `tick_chain` deliberately stays on plain `upsert` —
        // rewinding is its whole purpose.
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
            // This tick holds the chain's advisory lock, so nothing else
            // should be able to move this cursor. A rejected advance means a
            // second writer got the lock — the rows above were still written,
            // so the damage is duplicate work rather than loss, but it must
            // not pass silently.
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

    /// Rewind a cursor that points past the end of `raw_events` — the table
    /// was truncated or re-ingested under us, and every read would come back
    /// empty forever otherwise.
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
        // instead of polling the lock at full speed.
        let leader = self.locks.is_leader(chain_id).await?;
        // Gauged on both branches: failover is otherwise only visible as one
        // replica going quiet, which is indistinguishable from a stall.
        metrics::gauge!(
            shared::metrics::name::CHAIN_LEADER,
            "chain_id" => chain_id.to_string(),
        )
        .set(if leader { 1.0 } else { 0.0 });
        if !leader {
            return Ok(TickProgress::Idle);
        }

        // Retract before reading. Replacement rows for a reorged range come
        // back with fresh, higher ids and replay on their own, but the notes
        // and nullifiers derived from the *deleted* rows sit below the cursor
        // where nothing revisits them — leaving the tree describing a branch
        // that no longer exists. Applying the reorg log first drops those and
        // rewinds the cursor so the replay rebuilds them.
        // Retracting rewinds the cursor, so the replay is work queued right
        // now — come straight back rather than sleeping on it.
        let reorgs = database::reorg::apply_pending(&self.pool, NAME, chain_id).await?;
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
            // driver at zero delay against a tx it cannot yet commit.
            Planned::Incomplete { rows } => {
                self.stalls.record_idle(chain_id, after, rows).await;
                Ok(TickProgress::Idle)
            }
            Planned::Ready { plan, window_full } => {
                self.commit(chain_id, plan).await?;
                self.stalls.clear(chain_id).await;
                Ok(TickProgress::advanced(window_full))
            }
        }
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        match self.cursors.list_chain_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                // An empty list is indistinguishable from "no chains
                // configured", so the loop would idle silently.
                warn!(error = %e, "list_chain_ids failed; skipping this round");
                Vec::new()
            }
        }
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

/// `DepositFlushed` topic1 = deposit id, 32B big-endian.
fn flushed_deposit_ids(rows: &[RawEventRow]) -> Vec<Vec<u8>> {
    let flushed = EventKind::DepositFlushed.as_i16();
    let mut ids: Vec<Vec<u8>> = rows
        .iter()
        .filter(|r| r.event_kind == flushed)
        .filter_map(|r| r.topics.get(1).cloned())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn decode_escrowed(row: &RawEventRow) -> Option<(String, LeafPayload)> {
    let log = LogData::new_unchecked(
        row.topics.iter().map(|t| B256::from_slice(t)).collect(),
        row.data.clone().into(),
    );
    let ev = DepositEscrowed::decode_log_data(&log, true).ok()?;
    let payload = LeafPayload {
        cm: ev.cm.0.to_vec(),
        clue_rx: ev.clueRx,
        clue_ry: ev.clueRy,
        eph_pub_x: ev.ephPubX,
        eph_pub_y: ev.ephPubY,
        ciphertext: ev.ciphertext.to_vec(),
        cv_dep_x: ev.cvDepX,
        cv_dep_y: ev.cvDepY,
    };
    Some((U256::from(ev.id).to_string(), payload))
}

/// How long each chain has been parked on the same cursor.
///
/// Deferring a tx is normal for one tick and a silent outage after a thousand;
/// this is what tells the two apart. Nothing else would: the tick returns
/// `Ok(())` either way.
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;
    use alloy::sol_types::SolEvent;
    use chain_types::abi::DepositFlushed;

    fn row(id: i64, kind: EventKind, topics: Vec<Vec<u8>>, data: Vec<u8>) -> RawEventRow {
        RawEventRow {
            id,
            chain_id: 1,
            block_number: 10,
            block_hash: vec![0xaa; 32],
            block_ts: 1_700_000_000,
            tx_hash: vec![0x01; 32],
            log_index: id as i32,
            event_kind: kind.as_i16(),
            topics,
            data,
        }
    }

    fn flushed_row(id: i64, deposit_id: u64) -> RawEventRow {
        let log = DepositFlushed {
            id: U256::from(deposit_id),
            cm: B256::repeat_byte(0x11),
        }
        .encode_log_data();
        row(
            id,
            EventKind::DepositFlushed,
            log.topics().iter().map(|t| t.0.to_vec()).collect(),
            log.data.to_vec(),
        )
    }

    #[test]
    fn flushed_deposit_ids_reads_the_indexed_topic_and_dedupes() {
        let rows = [
            flushed_row(1, 9),
            // Not a flush: must not contribute a lookup key.
            row(2, EventKind::RootAdvanced, vec![vec![0xff; 32]], Vec::new()),
            flushed_row(3, 7),
            // The same deposit flushed twice in one window is one lookup.
            flushed_row(4, 9),
        ];

        let ids = flushed_deposit_ids(&rows);

        // topics[0] is the event signature; the id is topics[1], 32B BE.
        let expect = |n: u64| U256::from(n).to_be_bytes::<32>().to_vec();
        assert_eq!(ids.len(), 2, "sorted and deduped");
        assert!(ids.contains(&expect(7)) && ids.contains(&expect(9)));
    }

    #[test]
    fn flushed_deposit_ids_tolerates_a_log_with_no_indexed_topic() {
        // A malformed row must be skipped, not panic on `topics[1]`.
        let rows = [row(1, EventKind::DepositFlushed, Vec::new(), Vec::new())];

        assert!(flushed_deposit_ids(&rows).is_empty());
    }

    #[test]
    fn decode_escrowed_keys_the_payload_by_decimal_deposit_id() {
        // `plan_commit` looks the payload up by `U256::to_string()`, so the
        // key this produces has to be decimal, not hex or big-endian bytes.
        let ev = chain_types::abi::DepositEscrowed {
            id: U256::from(42u64),
            payer: Default::default(),
            recipient: Default::default(),
            publicAssetId: 0,
            publicIn: 0,
            feeBpsAtSubmit: 0,
            cm: B256::repeat_byte(0xcc),
            cvDepX: U256::ZERO,
            cvDepY: U256::ZERO,
            rcv: U256::ZERO,
            clueRx: U256::from(1u64),
            clueRy: U256::from(2u64),
            ephPubX: U256::ZERO,
            ephPubY: U256::ZERO,
            ciphertext: Bytes::from(vec![0x00, 0x07]),
        };
        let log = ev.encode_log_data();
        let stored = row(
            1,
            EventKind::DepositEscrowed,
            log.topics().iter().map(|t| t.0.to_vec()).collect(),
            log.data.to_vec(),
        );

        let (id, payload) = decode_escrowed(&stored).expect("round-trips");

        assert_eq!(id, "42");
        assert_eq!(payload.cm, vec![0xcc; 32]);
        assert_eq!(payload.ciphertext, vec![0x00, 0x07]);
    }

    #[test]
    fn decode_escrowed_rejects_a_row_that_is_not_a_deposit_escrowed() {
        let junk = row(
            1,
            EventKind::DepositEscrowed,
            vec![vec![0xff; 32]],
            Vec::new(),
        );

        assert!(decode_escrowed(&junk).is_none());
    }
}
