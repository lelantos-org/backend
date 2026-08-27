use crate::domain::error::IngesterError;
use crate::domain::models::{BlockCursor, RawEvent};
use crate::repositories::{AtomicWriteRepo, ChainStateRepo};
use shared::metrics::{ingest_stage, timed_ingest_stage};
use std::sync::Arc;

pub struct IngestService {
    writes: Arc<dyn AtomicWriteRepo>,
    chain_state: Arc<dyn ChainStateRepo>,
}

impl IngestService {
    pub fn new(writes: Arc<dyn AtomicWriteRepo>, chain_state: Arc<dyn ChainStateRepo>) -> Self {
        Self {
            writes,
            chain_state,
        }
    }

    /// Insert rows, advance the cursor and announce the append. The single
    /// commit path for both the live tick and the backfill. `last_scanned` is the
    /// upper bound of what was scanned, whether or not any rows fell in that
    /// range.
    ///
    /// Returns the number of rows inserted rather than decoded. Replayed ranges
    /// hit the unique index and insert nothing, so reporting the decoded count
    /// would make every replay look like fresh ingest.
    pub async fn commit_batch(
        &self,
        chain_id: i64,
        rows: &[RawEvent],
        last_scanned: i64,
    ) -> Result<usize, IngesterError> {
        // An empty batch contains no verified block, so it may move only the scan
        // watermark. Writing `last_block = last_scanned` with an empty hash would
        // leave the reorg anchor at a height that was never checked, paired with
        // bytes that are not a hash.
        let Some(cursor) = Self::cursor_for(chain_id, rows, last_scanned) else {
            self.advance_empty(chain_id, last_scanned).await?;
            return Ok(0);
        };

        // The wake-up rides the same transaction, so there is nothing to do
        // after it: Postgres queues a NOTIFY until commit, which makes the
        // announcement exactly as durable as the rows it announces.
        timed_ingest_stage(
            ingest_stage::COMMIT,
            chain_id,
            self.writes.commit_batch(rows, &cursor),
        )
        .await
    }

    pub async fn advance_empty(&self, chain_id: i64, scanned: i64) -> Result<(), IngesterError> {
        timed_ingest_stage(
            ingest_stage::COMMIT,
            chain_id,
            self.chain_state.advance_scanned(chain_id, scanned),
        )
        .await
    }

    /// The cursor a batch justifies, or `None` when it verifies no block.
    ///
    /// The anchor is the highest block present in the rows rather than the top of
    /// the scanned range: only a block a log was observed in has a hash that can
    /// be checked later.
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

    /// An empty batch has nothing to anchor to. Synthesising one would leave the
    /// reorg check walking back from an unverified height.
    #[test]
    fn an_empty_batch_justifies_no_cursor() {
        assert!(IngestService::cursor_for(1, &[], 500).is_none());
    }

    /// The anchor is the highest block seen, with that block's hash rather than
    /// the hash of whichever row came last.
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
