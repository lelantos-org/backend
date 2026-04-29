use crate::schema::*;
use diesel::prelude::*;

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = raw_events, primary_key(id))]
pub struct RawEventRow {
    pub id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub event_kind: i16,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = raw_events)]
pub struct NewRawEvent<'a> {
    pub chain_id: i64,
    pub block_number: i64,
    pub block_hash: &'a [u8],
    pub block_ts: i64,
    pub tx_hash: &'a [u8],
    pub log_index: i32,
    pub event_kind: i16,
    pub topics: Vec<Vec<u8>>,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, AsChangeset)]
#[diesel(table_name = chain_state, primary_key(chain_id))]
pub struct ChainStateRow {
    pub chain_id: i64,
    pub last_block: i64,
    pub last_block_hash: Vec<u8>,
    pub last_scanned_block: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = consumer_cursors, primary_key(name, chain_id))]
pub struct ConsumerCursorRow {
    pub name: String,
    pub chain_id: i64,
    pub last_event_id: i64,
    pub last_block_number: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
