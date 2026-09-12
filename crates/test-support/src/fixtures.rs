//! Row and log builders shared by the fixture-replay tests.
//!
//! These seed `raw_events` the way the ingester would, so an indexer's consume
//! tick can be driven without running the ingester. They were duplicated
//! verbatim in every replay test before living here.

use alloy::primitives::{Address, B256, LogData};
use alloy::rpc::types::eth::Log;
use database::DbPool;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use shared::entities::EventKind;

/// The MASP address every synthetic log is emitted from.
///
/// Arbitrary, but fixed: a decoder keys on topic0 rather than the emitter, so
/// the value only has to be stable across a test's logs.
pub const POOL_ADDR: &str = "0x0000000000000000000000000000000000000abc";

pub fn pool_addr() -> Address {
    POOL_ADDR.parse().unwrap()
}

/// One synthetic log, filled in as a node would report it.
///
/// `block_ts` is explicit rather than derived from `block_n`: the event-age
/// metric and the reorg anchor both read it, so a test that cares about either
/// needs to choose it.
pub fn build_log(log_data: LogData, block_n: u64, block_ts: u64, tx_byte: u8, log_idx: u64) -> Log {
    Log {
        inner: alloy::primitives::Log {
            address: pool_addr(),
            data: log_data,
        },
        block_hash: Some(B256::repeat_byte(0xaa)),
        block_number: Some(block_n),
        block_timestamp: Some(block_ts),
        transaction_hash: Some(B256::repeat_byte(tx_byte)),
        transaction_index: Some(0),
        log_index: Some(log_idx),
        removed: false,
    }
}

/// Mark `chain_id` as scanned from genesis.
///
/// A consume tick reads `chain_state` to find the chains it owns, so a fixture
/// that inserts events without this one row has nothing to tick.
pub async fn insert_chain_state(pool: &DbPool, chain_id: i64) {
    use database::schema::chain_state;
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(chain_state::table)
        .values((
            chain_state::chain_id.eq(chain_id),
            chain_state::last_block.eq(0i64),
            chain_state::last_block_hash.eq::<Vec<u8>>(vec![0u8; 32]),
            chain_state::last_scanned_block.eq(0i64),
        ))
        .execute(&mut conn)
        .await
        .unwrap();
}

#[derive(Insertable)]
#[diesel(table_name = database::schema::raw_events)]
struct InsertableRawEvent {
    chain_id: i64,
    block_number: i64,
    block_hash: Vec<u8>,
    block_ts: i64,
    tx_hash: Vec<u8>,
    log_index: i32,
    event_kind: i16,
    topics: Vec<Vec<u8>>,
    data: Vec<u8>,
}

/// Store `log` as the ingester would, under `kind`.
///
/// The kind is passed rather than derived from topic0 so a test can deliberately
/// file a log under the wrong one.
pub async fn insert_log(pool: &DbPool, chain_id: i64, log: &Log, kind: EventKind) {
    use database::schema::raw_events;
    let topics: Vec<Vec<u8>> = log.topics().iter().map(|t| t.0.to_vec()).collect();
    let row = InsertableRawEvent {
        chain_id,
        block_number: log.block_number.unwrap() as i64,
        block_hash: log.block_hash.unwrap().0.to_vec(),
        block_ts: log.block_timestamp.unwrap() as i64,
        tx_hash: log.transaction_hash.unwrap().0.to_vec(),
        log_index: log.log_index.unwrap() as i32,
        event_kind: kind.as_i16(),
        topics,
        data: log.data().data.to_vec(),
    };
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(raw_events::table)
        .values(&row)
        .execute(&mut conn)
        .await
        .unwrap();
}
