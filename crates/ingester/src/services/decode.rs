use crate::domain::models::RawEvent;
use alloy::primitives::B256;
use alloy::rpc::types::eth::Log;
use chain_types::decode::event_kind_from_topic0;
use std::collections::HashMap;

pub fn logs_to_rows(chain_id: i64, logs: Vec<Log>, block_ts: &HashMap<u64, u64>) -> Vec<RawEvent> {
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
        let ts = block_ts.get(&block_number).copied().unwrap_or(0);
        let topics: Vec<Vec<u8>> = log.topics().iter().map(|t: &B256| t.0.to_vec()).collect();
        out.push(RawEvent {
            chain_id,
            block_number: block_number as i64,
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
