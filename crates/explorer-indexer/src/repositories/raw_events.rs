use crate::error::ExplorerIndexerError;
use database::DbPool;
use database::schema::raw_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = raw_events)]
pub struct RawEventRow {
    pub id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    /// Solidity's `block.number` for this block, NULL for rows ingested before
    /// the column existed. Differs from `block_number` only on Arbitrum.
    pub evm_block_number: Option<i64>,
    pub block_hash: Vec<u8>,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub event_kind: i16,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

pub async fn batch_after(
    pool: &DbPool,
    chain_id: i64,
    after_id: i64,
    kinds: &[i16],
    limit: i64,
) -> Result<Vec<RawEventRow>, ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(raw_events::table
        .filter(raw_events::chain_id.eq(chain_id))
        .filter(raw_events::id.gt(after_id))
        .filter(raw_events::event_kind.eq_any(kinds))
        .order(raw_events::id.asc())
        .limit(limit)
        .select(RawEventRow::as_select())
        .load(&mut conn)
        .await?)
}

pub async fn siblings_by_tx(
    pool: &DbPool,
    chain_id: i64,
    tx_hash: &[u8],
) -> Result<Vec<RawEventRow>, ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(raw_events::table
        .filter(raw_events::chain_id.eq(chain_id))
        .filter(raw_events::tx_hash.eq(tx_hash))
        .order(raw_events::log_index.asc())
        .select(RawEventRow::as_select())
        .load(&mut conn)
        .await?)
}

pub async fn max_id(pool: &DbPool, chain_id: i64) -> Result<i64, ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    let v: Option<i64> = raw_events::table
        .filter(raw_events::chain_id.eq(chain_id))
        .select(diesel::dsl::max(raw_events::id))
        .first(&mut conn)
        .await?;
    Ok(v.unwrap_or(0))
}
