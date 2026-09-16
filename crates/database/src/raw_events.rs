//! Shared read access to `raw_events`.
//!
//! The ingester writes this table and three consumers stream it: fmd-indexer,
//! protocol-indexer and explorer-indexer. The row shape and the queries live
//! here for the same reason [`crate::cursor`] does — every crate depends on one
//! source of truth rather than a copy that can drift from the schema beside it.
//!
//! Read-only. Writing `raw_events` is the ingester's alone.

use crate::schema::raw_events;
use crate::{DbConn, DbPool};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RawEventsError {
    #[error("pool: {0}")]
    Pool(String),
    #[error("query: {0}")]
    Query(#[from] diesel::result::Error),
}

pub type RawEventsResult<T> = Result<T, RawEventsError>;

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

async fn conn(pool: &DbPool) -> RawEventsResult<DbConn<'_>> {
    pool.get()
        .await
        .map_err(|e| RawEventsError::Pool(e.to_string()))
}

/// One window of events for `chain_id`, ascending by id.
///
/// `kinds` is the caller's `WHERE event_kind = ANY(...)`. Consumers derive it
/// from `EventKind::kinds_for` rather than listing members, because the cursor
/// only advances to the highest id among the kinds actually fetched — a kind
/// handled but not fetched leaves its table permanently empty.
pub async fn batch_after(
    pool: &DbPool,
    chain_id: i64,
    after_id: i64,
    kinds: &[i16],
    limit: i64,
) -> RawEventsResult<Vec<RawEventRow>> {
    let mut conn = conn(pool).await?;
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

/// The highest id written for `chain_id`, or 0 when there are none.
///
/// Consumers compare their cursor against this: one ahead of the table means the
/// events were re-ingested beneath it, and it resets rather than waiting for an
/// id that will never arrive.
pub async fn max_id(pool: &DbPool, chain_id: i64) -> RawEventsResult<i64> {
    let mut conn = conn(pool).await?;
    let v: Option<i64> = raw_events::table
        .filter(raw_events::chain_id.eq(chain_id))
        .select(diesel::dsl::max(raw_events::id))
        .first(&mut conn)
        .await?;
    Ok(v.unwrap_or(0))
}
