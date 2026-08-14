use crate::adapters::locks::ChainLocks;
use crate::domain::error::Result;
use crate::domain::pending::{EscrowedMap, EscrowedSlot, plan_commit};
use crate::repositories::cursor::{CursorRepo, UpsertCursor};
use crate::repositories::notes::NotesRepo;
use crate::repositories::raw_events::{RawEventRow, RawEventsRepo};
use crate::repositories::spent_nullifiers::SpentNullifiersRepo;
use alloy::primitives::U256;
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use chain_types::abi::DepositEscrowed;
use shared::entities::EventKind;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub const NAME: &str = "fmd";
const KINDS: [i16; 4] = [
    EventKind::NoteCreated as i16,
    EventKind::RootAdvanced as i16,
    EventKind::NullifierConsumed as i16,
    EventKind::DepositFlushed as i16,
];

#[async_trait]
pub trait ConsumeService: Send + Sync {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<()>;
    async fn list_chain_ids(&self) -> Vec<i64>;
}

pub struct ConsumeServiceImpl {
    cursors: Arc<dyn CursorRepo>,
    raw_events: Arc<dyn RawEventsRepo>,
    notes: Arc<dyn NotesRepo>,
    spent_nfs: Arc<dyn SpentNullifiersRepo>,
    locks: ChainLocks,
}

impl ConsumeServiceImpl {
    pub fn new(
        cursors: Arc<dyn CursorRepo>,
        raw_events: Arc<dyn RawEventsRepo>,
        notes: Arc<dyn NotesRepo>,
        spent_nfs: Arc<dyn SpentNullifiersRepo>,
        locks: ChainLocks,
    ) -> Self {
        Self {
            cursors,
            raw_events,
            notes,
            spent_nfs,
            locks,
        }
    }

    /// Collect deposit_ids referenced by DepositFlushed events, look up their
    /// originating DepositEscrowed events, decode the leaf's cm + aux, and
    /// return the lookup map keyed by deposit_id (decimal).
    async fn resolve_escrowed(&self, chain_id: i64, rows: &[RawEventRow]) -> Result<EscrowedMap> {
        let kind_flushed = EventKind::DepositFlushed.as_i16();
        // DepositFlushed indexed topic1 = id (32B big-endian).
        let mut deposit_topics: Vec<Vec<u8>> = Vec::new();
        for r in rows {
            if r.event_kind != kind_flushed {
                continue;
            }
            if r.topics.len() >= 2 {
                deposit_topics.push(r.topics[1].clone());
            }
        }
        if deposit_topics.is_empty() {
            return Ok(HashMap::new());
        }
        deposit_topics.sort();
        deposit_topics.dedup();

        let escrowed_rows = self
            .raw_events
            .fetch_escrowed_by_ids(chain_id, &deposit_topics)
            .await?;
        let mut out: EscrowedMap = HashMap::new();
        for r in escrowed_rows {
            let log = alloy::primitives::LogData::new_unchecked(
                r.topics
                    .iter()
                    .map(|t| alloy::primitives::B256::from_slice(t))
                    .collect(),
                r.data.clone().into(),
            );
            let ev = match DepositEscrowed::decode_log_data(&log, true) {
                Ok(ev) => ev,
                Err(e) => {
                    warn!(chain_id, error = %e, "decode DepositEscrowed failed; skipping");
                    continue;
                }
            };
            let key = U256::from(ev.id).to_string();
            let slot = EscrowedSlot {
                cm: ev.cm.0.to_vec(),
                clue_rx: ev.clueRx,
                clue_ry: ev.clueRy,
                eph_pub_x: ev.ephPubX,
                eph_pub_y: ev.ephPubY,
                ciphertext: ev.ciphertext.to_vec(),
                cv_dep_x: ev.cvDepX,
                cv_dep_y: ev.cvDepY,
            };
            out.insert(key, slot);
        }
        Ok(out)
    }
}

#[async_trait]
impl ConsumeService for ConsumeServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<()> {
        // Every write below assumes this process is the only one consuming
        // this chain: the cursor read-modify-write, and `spent_nullifiers.seq`
        // ordinal assignment which silently gaps under a concurrent writer.
        if !self.locks.is_leader(chain_id).await? {
            return Ok(());
        }

        let (after, _last_block) = self.cursors.fetch(NAME, chain_id).await?;
        let max_id = self.raw_events.max_id(chain_id).await?;
        if after > max_id {
            warn!(chain_id, "cursor ahead of raw_events.max_id; reset to 0");
            self.cursors
                .upsert(UpsertCursor {
                    name: NAME.to_string(),
                    chain_id,
                    last_event_id: 0,
                    last_block_number: 0,
                })
                .await?;
            return Ok(());
        }

        let rows = self
            .raw_events
            .batch_after(chain_id, after, &KINDS, batch)
            .await?;
        if rows.is_empty() {
            return Ok(());
        }

        let escrowed = self.resolve_escrowed(chain_id, &rows).await?;

        let plan = match plan_commit(&rows, chain_id, after, &escrowed)? {
            Some(p) => p,
            None => return Ok(()),
        };

        self.notes.insert_batch(&plan.notes).await?;
        self.spent_nfs.insert_batch(&plan.spent_nfs).await?;
        // Monotonic: never drag the cursor backwards if a peer is ahead. The
        // reset above deliberately stays on plain `upsert` — rewinding to 0 is
        // its whole purpose.
        self.cursors
            .upsert_monotonic(UpsertCursor {
                name: NAME.to_string(),
                chain_id,
                last_event_id: plan.last_event_id,
                last_block_number: plan.last_block_number,
            })
            .await?;
        let notes_len = plan.notes.len();
        let spent_len = plan.spent_nfs.len();
        debug!(
            chain_id,
            notes = notes_len,
            spent_nfs = spent_len,
            last_id = plan.last_event_id,
            last_block = plan.last_block_number,
            "consume commit"
        );
        if notes_len > 0 || spent_len > 0 {
            info!(
                chain_id,
                notes = notes_len,
                spent_nfs = spent_len,
                last_block = plan.last_block_number,
                "consume committed events"
            );
        }
        Ok(())
    }

    async fn list_chain_ids(&self) -> Vec<i64> {
        self.cursors.list_chain_ids().await.unwrap_or_default()
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
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<()> {
        ConsumeService::tick_chain(self, chain_id, batch)
            .await
            .map_err(Into::into)
    }
}
