use crate::domain::error::IngesterError;
use crate::domain::models::{BlockCursor, RawEvent};
use crate::repositories::{AtomicWriteRepo, ChainStateRepo, RawEventRepo};
use std::sync::Arc;
use tracing::warn;

pub struct IngestService {
    writes: Arc<dyn AtomicWriteRepo>,
    raw_events: Arc<dyn RawEventRepo>,
    chain_state: Arc<dyn ChainStateRepo>,
}

impl IngestService {
    pub fn new(
        writes: Arc<dyn AtomicWriteRepo>,
        raw_events: Arc<dyn RawEventRepo>,
        chain_state: Arc<dyn ChainStateRepo>,
    ) -> Self {
        Self {
            writes,
            raw_events,
            chain_state,
        }
    }

    /// Insert rows, advance the cursor, fire notify. The single commit path
    /// for both the live tick and the backfill. `last_scanned` is the upper
    /// bound of what was just scanned, independent of whether any rows fell in
    /// that range.
    ///
    /// Returns the number of rows actually inserted — not the number decoded.
    /// Replayed ranges hit the unique index and insert nothing, and reporting
    /// the decoded count there makes every replay look like fresh ingest.
    pub async fn commit_batch(
        &self,
        chain_id: i64,
        rows: &[RawEvent],
        last_scanned: i64,
    ) -> Result<usize, IngesterError> {
        // An empty batch contains no verified block, so it may move only the
        // scan watermark. Writing `last_block = last_scanned` with an empty
        // hash — as this used to — leaves the reorg anchor pointing at a
        // height that was never checked, paired with a hash that is not one.
        let Some(cursor) = Self::cursor_for(chain_id, rows, last_scanned) else {
            self.advance_empty(chain_id, last_scanned).await?;
            return Ok(0);
        };

        let inserted = self.writes.commit_batch(rows, &cursor).await?;

        // The rows are committed at this point. Failing the batch because the
        // wake-up failed would roll nothing back and, under the retry policy,
        // count against the chain's failure budget for no reason.
        if let Err(e) = self.raw_events.notify_appended(chain_id).await {
            warn!(chain_id, "notify failed after a successful commit: {}", e);
        }
        Ok(inserted)
    }

    pub async fn advance_empty(&self, chain_id: i64, scanned: i64) -> Result<(), IngesterError> {
        self.chain_state.advance_scanned(chain_id, scanned).await
    }

    /// The cursor a batch justifies, or `None` when it verifies no block.
    ///
    /// The anchor is the highest block *present in the rows*, never the top of
    /// the scanned range: only a block we actually saw a log in has a hash we
    /// can check later.
    fn cursor_for(chain_id: i64, rows: &[RawEvent], last_scanned: i64) -> Option<BlockCursor> {
        let anchor = rows.iter().max_by_key(|r| r.block_number)?;
        Some(BlockCursor {
            chain_id,
            last_block: anchor.block_number,
            last_block_hash: anchor.block_hash.clone(),
            last_scanned_block: last_scanned,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(block_number: i64, hash: u8) -> RawEvent {
        RawEvent {
            chain_id: 1,
            block_number,
            evm_block_number: block_number,
            block_hash: vec![hash; 32],
            block_ts: 0,
            tx_hash: vec![0; 32],
            log_index: 0,
            event_kind: 0,
            topics: Vec::new(),
            data: Vec::new(),
        }
    }

    /// An empty batch has nothing to anchor to. Inventing one leaves the reorg
    /// check walking back from a height it never verified.
    #[test]
    fn an_empty_batch_justifies_no_cursor() {
        assert!(IngestService::cursor_for(1, &[], 500).is_none());
    }

    /// The anchor is the highest block seen, with *that block's* hash — not
    /// whichever row happened to come last.
    #[test]
    fn the_anchor_is_the_highest_block_in_the_batch() {
        let rows = [row(10, 0xaa), row(12, 0xcc), row(11, 0xbb)];
        let cursor = IngestService::cursor_for(1, &rows, 99).expect("rows present");
        assert_eq!(cursor.last_block, 12);
        assert_eq!(cursor.last_block_hash, vec![0xcc; 32]);
        assert_eq!(
            cursor.last_scanned_block, 99,
            "the watermark is the scanned range, not the anchor"
        );
    }
}
