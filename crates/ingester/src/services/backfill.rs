//! Chunked, parallel catch-up.
//!
//! Used whenever the gap to tip is wider than a poll-interval tail can close.

use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::domain::error::IngesterError;
use crate::services::ingest::IngestService;
use crate::services::log_range::{LogWindow, fetch_rows};
use alloy::primitives::Address;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use tracing::info;

pub struct BackfillService {
    rpc: DynRpc,
    ingest: Arc<IngestService>,
    log_window: Arc<LogWindow>,
}

/// One unit of catch-up work: an inclusive block range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chunk {
    from: u64,
    to: u64,
}

/// Split `[from, to]` into chunks of at most `size` blocks.
///
/// A free function so the boundary arithmetic, including the tail clip and the
/// `size == 0` case that would panic `step_by`, is testable directly.
fn chunks(from: u64, to: u64, size: u64) -> Vec<Chunk> {
    if from > to {
        return Vec::new();
    }
    let size = size.max(1);
    (from..=to)
        .step_by(size as usize)
        .map(|start| Chunk {
            from: start,
            to: start.saturating_add(size - 1).min(to),
        })
        .collect()
}

impl BackfillService {
    pub fn new(rpc: DynRpc, ingest: Arc<IngestService>, log_window: Arc<LogWindow>) -> Self {
        Self {
            rpc,
            ingest,
            log_window,
        }
    }

    pub async fn run(
        &self,
        cfg: &ChainConfig,
        pool_addr: Address,
        from: u64,
        to: u64,
    ) -> Result<(), IngesterError> {
        let chain_id = cfg.chain_id;
        let chunks = chunks(from, to, cfg.chunk_blocks);
        if chunks.is_empty() {
            return Ok(());
        }
        info!(
            chain_id,
            from,
            to,
            chunks = chunks.len(),
            chunk_blocks = cfg.chunk_blocks,
            concurrency = cfg.backfill_concurrency,
            "backfill start"
        );

        // Everything up to the write runs inside the concurrent stage. Resolving
        // block metadata is one `eth_getBlockByNumber` per distinct block, so
        // leaving it in the drain loop meant the dominant cost of a chunk ran
        // strictly in series behind the log fetches, and the fetch stream was not
        // polled forward while it did.
        // Everything up to the write runs inside the concurrent stage. Resolving
        // block metadata is one `eth_getBlockByNumber` per distinct block, so
        // leaving it in the drain loop meant the dominant cost of a chunk ran
        // strictly in series behind the log fetches, and the fetch stream was not
        // polled forward while it did.
        let rpc = &self.rpc;
        let log_window = &self.log_window;
        let mut prepared = stream::iter(chunks.into_iter().map(|chunk| async move {
            let rows =
                fetch_rows(rpc, log_window, chain_id, pool_addr, chunk.from, chunk.to).await?;
            Ok::<_, IngesterError>((chunk, rows))
        }))
        .buffered(cfg.backfill_concurrency.max(1));

        // `buffered` yields in submission order, so the cursor moves only forward
        // even though the chunks complete out of order. The drain does nothing
        // but write, which is the one step that must stay ordered.
        while let Some(result) = prepared.next().await {
            let (chunk, rows) = result?;
            let inserted = self
                .ingest
                .commit_batch(chain_id, &rows, chunk.to as i64)
                .await?;
            info!(
                chain_id,
                from = chunk.from,
                to = chunk.to,
                inserted,
                "backfill chunk"
            );
        }
        info!(chain_id, from, to, "backfill done");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_tile_the_range_exactly() {
        let cs = chunks(10, 34, 10);
        assert_eq!(
            cs,
            vec![
                Chunk { from: 10, to: 19 },
                Chunk { from: 20, to: 29 },
                Chunk { from: 30, to: 34 },
            ],
            "no gaps, no overlap, tail clipped to `to`"
        );
    }

    #[test]
    fn a_single_block_range_is_one_chunk() {
        assert_eq!(chunks(7, 7, 50), vec![Chunk { from: 7, to: 7 }]);
    }

    #[test]
    fn an_inverted_range_yields_nothing() {
        assert!(chunks(10, 9, 50).is_empty());
    }

    /// `Iterator::step_by(0)` panics. Config validation rejects a zero size, and
    /// this guards the call site as well.
    #[test]
    fn a_zero_chunk_size_does_not_panic() {
        assert_eq!(chunks(1, 3, 0).len(), 3);
    }
}
