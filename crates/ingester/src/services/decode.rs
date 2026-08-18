use crate::adapters::rpc::BlockMeta;
use crate::domain::error::{IngesterError, RpcError};
use crate::domain::models::RawEvent;
use alloy::primitives::B256;
use alloy::rpc::types::eth::Log;
use chain_types::decode::event_kind_from_topic0;
use std::collections::HashMap;
use tracing::warn;

/// Turn provider logs into insertable rows.
///
/// Errors rather than substituting defaults when a log's block metadata is
/// missing: a zero `block_ts` and an L2 height in `evm_block_number` are both
/// silently wrong, and on Arbitrum the latter is exactly what makes every
/// later `flushBatch` revert `DigestMismatch`.
pub fn logs_to_rows(
    chain_id: i64,
    logs: Vec<Log>,
    block_meta: &HashMap<u64, BlockMeta>,
) -> Result<Vec<RawEvent>, IngesterError> {
    let mut out = Vec::with_capacity(logs.len());
    for log in logs {
        // Some providers replay logs from orphaned blocks with this set.
        // Inserting them would present a dead branch as canonical.
        if log.removed {
            warn!(chain_id, "skipping log flagged removed");
            continue;
        }
        let Some(topic0) = log.topic0() else { continue };
        let Some(kind) = event_kind_from_topic0(topic0) else {
            continue;
        };
        let Some(block_number) = log.block_number else {
            continue;
        };
        let Some(block_hash) = log.block_hash else {
            continue;
        };
        let Some(tx_hash) = log.transaction_hash else {
            continue;
        };
        let Some(log_index) = log.log_index else {
            continue;
        };
        let meta = block_meta
            .get(&block_number)
            .copied()
            .ok_or(IngesterError::Rpc(RpcError::BlockMissing(block_number)))?;
        let topics: Vec<Vec<u8>> = log.topics().iter().map(|t: &B256| t.0.to_vec()).collect();
        out.push(RawEvent {
            chain_id,
            block_number: block_number as i64,
            evm_block_number: meta.evm_block_number as i64,
            block_hash: block_hash.0.to_vec(),
            block_ts: meta.timestamp as i64,
            tx_hash: tx_hash.0.to_vec(),
            log_index: log_index as i32,
            event_kind: kind.as_i16(),
            topics,
            data: log.data().data.to_vec(),
        });
    }
    Ok(out)
}

/// The distinct blocks a batch of logs touches.
///
/// Both ingest paths need this to resolve block metadata, and both used to
/// open-code the same `filter_map`/`HashSet`/collect dance.
pub fn distinct_blocks(logs: &[Log]) -> Vec<u64> {
    logs.iter()
        .filter_map(|l| l.block_number)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, B256, Bytes, LogData};
    use alloy::rpc::types::eth::Log as RpcLog;
    use chain_types::decode::known_signatures;

    /// Build a log carrying a recognised topic0 so `logs_to_rows` keeps it.
    fn log_at(block_number: u64) -> RpcLog {
        let topic0 = known_signatures()
            .first()
            .copied()
            .expect("at least one known event signature");
        RpcLog {
            inner: alloy::primitives::Log {
                address: Address::ZERO,
                data: LogData::new_unchecked(vec![topic0], Bytes::new()),
            },
            block_number: Some(block_number),
            block_hash: Some(B256::ZERO),
            transaction_hash: Some(B256::ZERO),
            log_index: Some(0),
            transaction_index: Some(0),
            block_timestamp: None,
            removed: false,
        }
    }

    /// Arbitrum reports the L1 height from `block.number`, and MASP hashes that
    /// into the deposit digest. Recording the L2 height instead makes every
    /// `flushBatch` revert `DigestMismatch`, so the two must stay distinct.
    #[test]
    fn keeps_the_evm_block_number_separate_from_the_chain_height() {
        let l2 = 495_232_834u64;
        let l1 = 25_769_577u64;
        let meta = HashMap::from([(
            l2,
            BlockMeta {
                timestamp: 1_786_906_840,
                evm_block_number: l1,
            },
        )]);

        let rows = logs_to_rows(42161, vec![log_at(l2)], &meta).expect("meta present");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].block_number, l2 as i64, "chain height is preserved");
        assert_eq!(
            rows[0].evm_block_number, l1 as i64,
            "digest must use what the EVM saw, not the L2 height"
        );
    }

    /// Ethereum and OP-stack chains report their own height, so the two values
    /// coincide and nothing downstream has to special-case them.
    #[test]
    fn evm_block_number_equals_chain_height_when_they_agree() {
        let n = 50_057_907u64;
        let meta = HashMap::from([(
            n,
            BlockMeta {
                timestamp: 1_786_905_161,
                evm_block_number: n,
            },
        )]);

        let rows = logs_to_rows(8453, vec![log_at(n)], &meta).expect("meta present");

        assert_eq!(rows[0].block_number, n as i64);
        assert_eq!(rows[0].evm_block_number, n as i64);
    }

    /// A missing block would previously commit `block_ts = 0` and the L2
    /// height as `evm_block_number` — both silently wrong, and the second is
    /// what makes Arbitrum `flushBatch` calls revert `DigestMismatch`.
    #[test]
    fn missing_block_metadata_is_an_error_not_a_default() {
        let meta = HashMap::new();
        let err = logs_to_rows(1, vec![log_at(99)], &meta).expect_err("must not default");
        assert!(
            matches!(err, IngesterError::Rpc(RpcError::BlockMissing(99))),
            "got {err:?}"
        );
    }

    /// `removed` marks a log from an orphaned block. Storing it would present
    /// a dead branch as canonical.
    #[test]
    fn skips_logs_flagged_removed() {
        let n = 7u64;
        let meta = HashMap::from([(
            n,
            BlockMeta {
                timestamp: 1,
                evm_block_number: n,
            },
        )]);
        let mut log = log_at(n);
        log.removed = true;

        let rows = logs_to_rows(1, vec![log], &meta).expect("no error");

        assert!(rows.is_empty(), "removed logs must not be stored");
    }
}
