//! Chunked, parallel catch-up.
//!
//! Used whenever the gap to tip is wider than a poll-interval tail can close.

use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::domain::error::IngesterError;
use crate::services::decode::{distinct_blocks, logs_to_rows};
use crate::services::ingest::IngestService;
use crate::services::log_range::fetch_adaptive;
use alloy::primitives::Address;
use alloy::rpc::types::eth::Log;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use tracing::info;

pub struct BackfillService {
    rpc: DynRpc,
    ingest: Arc<IngestService>,
}

/// One unit of catch-up work: an inclusive block range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chunk {
    from: u64,
    to: u64,
}

/// Split `[from, to]` into chunks of at most `size` blocks.
///
/// A free function so the boundary arithmetic — the off-by-one at the tail,
/// and the `size == 0` case that would panic `step_by` — is testable directly.
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
    pub fn new(rpc: DynRpc, ingest: Arc<IngestService>) -> Self {
        Self { rpc, ingest }
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

        let rpc = self.rpc.clone();
        let mut fetches = stream::iter(chunks.into_iter().map(|chunk| {
            let rpc = rpc.clone();
            async move {
                let logs = fetch_adaptive(&rpc, pool_addr, chunk.from, chunk.to).await?;
                Ok::<_, IngesterError>((chunk, logs))
            }
        }))
        .buffered(cfg.backfill_concurrency.max(1));

        // `buffered` yields in submission order, so the cursor only ever moves
        // forward even though the fetches complete out of order.
        while let Some(result) = fetches.next().await {
            let (chunk, logs) = result?;
            let inserted = self.commit(chain_id, chunk, logs).await?;
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

    async fn commit(
        &self,
        chain_id: i64,
        chunk: Chunk,
        logs: Vec<Log>,
    ) -> Result<usize, IngesterError> {
        let block_meta = self.rpc.fetch_block_meta(&distinct_blocks(&logs)).await?;
        let rows = logs_to_rows(chain_id, logs, &block_meta)?;
        self.ingest
            .commit_batch(chain_id, &rows, chunk.to as i64)
            .await
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

    /// `Iterator::step_by(0)` panics. Config validation rejects it, but this
    /// is where it would detonate, so it is defended here too.
    #[test]
    fn a_zero_chunk_size_does_not_panic() {
        assert_eq!(chunks(1, 3, 0).len(), 3);
    }
}
