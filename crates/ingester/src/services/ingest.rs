use crate::domain::error::IngesterError;
use crate::domain::models::{BlockCursor, RawEvent};
use crate::repositories::{ChainStateRepo, RawEventRepo};
use std::sync::Arc;

pub struct IngestService {
    raw_events: Arc<dyn RawEventRepo>,
    chain_state: Arc<dyn ChainStateRepo>,
}

impl IngestService {
    pub fn new(raw_events: Arc<dyn RawEventRepo>, chain_state: Arc<dyn ChainStateRepo>) -> Self {
        Self {
            raw_events,
            chain_state,
        }
    }

    /// Insert rows, advance cursor, fire notify. Single commit path used by both
    /// live tick and backfill. `last_scanned` is the upper bound of what was just
    /// scanned (independent of whether rows fell in that range).
    pub async fn commit_batch(
        &self,
        chain_id: i64,
        rows: &[RawEvent],
        last_scanned: i64,
    ) -> Result<(), IngesterError> {
        self.raw_events.insert_batch(rows).await?;
        let last_block = rows
            .iter()
            .map(|r| r.block_number)
            .max()
            .unwrap_or(last_scanned);
        let last_block_hash = rows
            .iter()
            .filter(|r| r.block_number == last_block)
            .map(|r| r.block_hash.clone())
            .next()
            .unwrap_or_default();
        self.chain_state
            .upsert(BlockCursor {
                chain_id,
                last_block,
                last_block_hash,
                last_scanned_block: last_scanned,
            })
            .await?;
        if !rows.is_empty() {
            self.raw_events.notify(chain_id).await?;
        }
        Ok(())
    }

    pub async fn advance_empty(&self, chain_id: i64, scanned: i64) -> Result<(), IngesterError> {
        self.chain_state.advance_scanned(chain_id, scanned).await
    }
}
