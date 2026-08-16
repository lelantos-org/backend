use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::domain::error::{IngesterError, RpcError};
use crate::services::decode::logs_to_rows;
use crate::services::ingest::IngestService;
use alloy::primitives::Address;
use alloy::rpc::types::eth::Log;
use futures::stream::{self, StreamExt};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::info;

pub struct BackfillService {
    rpc: DynRpc,
    ingest: Arc<IngestService>,
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
        let chunks: Vec<(u64, u64)> = (from..=to)
            .step_by(cfg.chunk_blocks as usize)
            .map(|s| (s, std::cmp::min(s + cfg.chunk_blocks - 1, to)))
            .collect();
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
        let mut stream = stream::iter(chunks.into_iter().map(|(s, e)| {
            let rpc = rpc.clone();
            async move {
                let logs = fetch_with_adaptive_range(&rpc, pool_addr, s, e).await?;
                Ok::<((u64, u64), Vec<Log>), IngesterError>(((s, e), logs))
            }
        }))
        .buffered(cfg.backfill_concurrency);

        while let Some(res) = stream.next().await {
            let ((cs, ce), logs) = res?;
            let block_numbers: Vec<u64> = logs
                .iter()
                .filter_map(|l| l.block_number)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let block_meta = self.rpc.fetch_block_meta(&block_numbers).await?;
            let rows = logs_to_rows(chain_id, logs, &block_meta);
            self.ingest.commit_batch(chain_id, &rows, ce as i64).await?;
            info!(chain_id, cs, ce, inserted = rows.len(), "backfill chunk");
        }
        info!(chain_id, from, to, "backfill done");
        Ok(())
    }
}

/// Probe a [from, to] window. On RangeTooLarge / ResponseTooLarge, halve.
/// Otherwise grow the window back up. Bubbles up other RPC errors.
async fn fetch_with_adaptive_range(
    rpc: &DynRpc,
    pool_addr: Address,
    from: u64,
    to: u64,
) -> Result<Vec<Log>, IngesterError> {
    let mut next_size = to - from + 1;
    let mut cursor = from;
    let mut acc = Vec::new();
    while cursor <= to {
        let end = std::cmp::min(cursor + next_size - 1, to);
        match rpc.fetch_logs(pool_addr, cursor, end).await {
            Ok(mut logs) => {
                acc.append(&mut logs);
                cursor = end + 1;
                let remaining = to.saturating_sub(cursor).saturating_add(1);
                next_size = std::cmp::min(next_size.saturating_mul(2), remaining);
            }
            Err(IngesterError::Rpc(RpcError::RangeTooLarge | RpcError::ResponseTooLarge))
                if next_size > 1 =>
            {
                next_size = std::cmp::max(next_size / 2, 1);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(acc)
}
