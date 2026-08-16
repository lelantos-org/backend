use crate::adapters::rpc::BlockMeta;
use crate::domain::models::RawEvent;
use alloy::primitives::B256;
use alloy::rpc::types::eth::Log;
use chain_types::decode::event_kind_from_topic0;
use std::collections::HashMap;

pub fn logs_to_rows(
    chain_id: i64,
    logs: Vec<Log>,
    block_meta: &HashMap<u64, BlockMeta>,
) -> Vec<RawEvent> {
    let mut out = Vec::with_capacity(logs.len());
    for log in logs {
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
        let meta = block_meta.get(&block_number).copied();
        let ts = meta.map(|m| m.timestamp).unwrap_or(0);
        // Fall back to the chain's own height: correct everywhere except
        // Arbitrum, and a missing block there is already an ingest failure.
        let evm_block_number = meta.map(|m| m.evm_block_number).unwrap_or(block_number);
        let topics: Vec<Vec<u8>> = log.topics().iter().map(|t: &B256| t.0.to_vec()).collect();
        out.push(RawEvent {
            chain_id,
            block_number: block_number as i64,
            evm_block_number: evm_block_number as i64,
            block_hash: block_hash.0.to_vec(),
            block_ts: ts as i64,
            tx_hash: tx_hash.0.to_vec(),
            log_index: log_index as i32,
            event_kind: kind.as_i16(),
            topics,
            data: log.data().data.to_vec(),
        });
    }
    out
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

        let rows = logs_to_rows(42161, vec![log_at(l2)], &meta);

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

        let rows = logs_to_rows(8453, vec![log_at(n)], &meta);

        assert_eq!(rows[0].block_number, n as i64);
        assert_eq!(rows[0].evm_block_number, n as i64);
    }
}
