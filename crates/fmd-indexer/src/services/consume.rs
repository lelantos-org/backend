use crate::domain::error::Result;
use crate::domain::pending::{EscrowedMap, EscrowedSlot, plan_commit};
use crate::repositories::cursor::{CursorRepo, UpsertCursor};
use crate::repositories::notes::NotesRepo;
use crate::repositories::raw_events::{RawEventRow, RawEventsRepo};
use crate::repositories::spent_nullifiers::SpentNullifiersRepo;
use alloy::primitives::U256;
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use chain_types::abi::IntentEscrowed;
use shared::entities::EventKind;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub const NAME: &str = "fmd";
const KINDS: [i16; 4] = [
    EventKind::NoteCreated as i16,
    EventKind::RootAdvanced as i16,
    EventKind::NullifierConsumed as i16,
    EventKind::IntentFlushed as i16,
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
}

impl ConsumeServiceImpl {
    pub fn new(
        cursors: Arc<dyn CursorRepo>,
        raw_events: Arc<dyn RawEventsRepo>,
        notes: Arc<dyn NotesRepo>,
        spent_nfs: Arc<dyn SpentNullifiersRepo>,
    ) -> Self {
        Self {
            cursors,
            raw_events,
            notes,
            spent_nfs,
        }
    }

    /// Collect intent_ids referenced by IntentFlushed events, look up their
    /// originating IntentEscrowed events, decode cm + aux for both output
    /// slots, and return the lookup map keyed by intent_id (decimal).
    async fn resolve_escrowed(&self, chain_id: i64, rows: &[RawEventRow]) -> Result<EscrowedMap> {
        let kind_flushed = EventKind::IntentFlushed.as_i16();
        // IntentFlushed indexed topic1 = id (32B big-endian).
        let mut intent_topics: Vec<Vec<u8>> = Vec::new();
        for r in rows {
            if r.event_kind != kind_flushed {
                continue;
            }
            if r.topics.len() >= 2 {
                intent_topics.push(r.topics[1].clone());
            }
        }
        if intent_topics.is_empty() {
            return Ok(HashMap::new());
        }
        intent_topics.sort();
        intent_topics.dedup();

        let escrowed_rows = self
            .raw_events
            .fetch_escrowed_by_ids(chain_id, &intent_topics)
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
            let ev = match IntentEscrowed::decode_log_data(&log, true) {
                Ok(ev) => ev,
                Err(e) => {
                    warn!(chain_id, error = %e, "decode IntentEscrowed failed; skipping");
                    continue;
                }
            };
            let key = U256::from(ev.id).to_string();
            let slots = [
                EscrowedSlot {
                    cm: ev.cm0.0.to_vec(),
                    clue_rx: ev.clueRx0,
                    clue_ry: ev.clueRy0,
                    eph_pub_x: ev.ephPubX0,
                    eph_pub_y: ev.ephPubY0,
                    ciphertext: ev.ciphertext0.to_vec(),
                    cv_dep_x: ev.cvDep0X,
                    cv_dep_y: ev.cvDep0Y,
                },
                EscrowedSlot {
                    cm: ev.cm1.0.to_vec(),
                    clue_rx: ev.clueRx1,
                    clue_ry: ev.clueRy1,
                    eph_pub_x: ev.ephPubX1,
                    eph_pub_y: ev.ephPubY1,
                    ciphertext: ev.ciphertext1.to_vec(),
                    cv_dep_x: ev.cvDep1X,
                    cv_dep_y: ev.cvDep1Y,
                },
            ];
            out.insert(key, slots);
        }
        Ok(out)
    }
}

#[async_trait]
impl ConsumeService for ConsumeServiceImpl {
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> Result<()> {
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
        self.cursors
            .upsert(UpsertCursor {
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
