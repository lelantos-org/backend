use crate::error::ExplorerIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::asset_yield;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// One asset's venue binding, from `YieldAssetAdded`.
///
/// Creating this row is what makes an asset yield-bearing; the contract has no
/// event that undoes it.
#[derive(Debug, Clone, Insertable, AsChangeset)]
#[diesel(table_name = asset_yield)]
pub struct UpsertBinding {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub venue: Vec<u8>,
    pub buffer_bps: i16,
    pub perf_bps: i16,
}

/// Idempotent so a cursor rewind can replay the binding.
pub async fn upsert_binding(pool: &DbPool, row: UpsertBinding) -> Result<(), ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    diesel::insert_into(asset_yield::table)
        .values(&row)
        .on_conflict((asset_yield::chain_id, asset_yield::asset_id_u64))
        .do_update()
        .set(&row)
        .execute(&mut conn)
        .await?;
    Ok(())
}

/// An `UPDATE` rather than an upsert, unlike `assets::upsert_fee`.
///
/// `setYieldParams` reverts unless the asset already carries a venue, so
/// `YieldAssetAdded` always precedes this in the log stream and a replay
/// preserves that order. There is no case where the row is missing, and `venue`
/// is `NOT NULL`, so there is no placeholder to invent.
pub async fn set_params(
    pool: &DbPool,
    chain_id: i64,
    asset_id_u64: i64,
    buffer_bps: i16,
    perf_bps: i16,
) -> Result<(), ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    diesel::update(asset_yield::table.find((chain_id, asset_id_u64)))
        .set((
            asset_yield::buffer_bps.eq(buffer_bps),
            asset_yield::perf_bps.eq(perf_bps),
        ))
        .execute(&mut conn)
        .await?;
    Ok(())
}

pub async fn set_halted(
    pool: &DbPool,
    chain_id: i64,
    asset_id_u64: i64,
    halted: bool,
) -> Result<(), ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    diesel::update(asset_yield::table.find((chain_id, asset_id_u64)))
        .set(asset_yield::halted.eq(halted))
        .execute(&mut conn)
        .await?;
    Ok(())
}

/// The polled half of the row, from `MASP.yieldState`.
///
/// `updated_at` is not a field: it is set to the database's `now()` in the
/// query, so the freshness stamp comes from one clock rather than from whichever
/// indexer replica wrote the row.
#[derive(Debug, Clone, PartialEq, AsChangeset)]
#[diesel(table_name = asset_yield)]
pub struct UpdateState {
    pub total_normalized: BigDecimal,
    pub accrued_fee_normalized: BigDecimal,
    pub idle: BigDecimal,
    pub last_idx: BigDecimal,
    pub gross: BigDecimal,
    pub index_ray: BigDecimal,
    pub block_number: i64,
}

pub async fn update_state(
    pool: &DbPool,
    chain_id: i64,
    asset_id_u64: i64,
    row: UpdateState,
) -> Result<(), ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    diesel::update(asset_yield::table.find((chain_id, asset_id_u64)))
        .set((&row, asset_yield::updated_at.eq(diesel::dsl::now)))
        .execute(&mut conn)
        .await?;
    Ok(())
}

/// One yield asset the poller has to refresh, with the venue it reads through.
#[derive(Debug, Clone, Queryable)]
pub struct YieldAssetRef {
    pub asset_id_u64: i64,
    pub venue: Vec<u8>,
}

/// Every yield-bearing asset on one chain.
pub async fn list_for_chain(
    pool: &DbPool,
    chain_id: i64,
) -> Result<Vec<YieldAssetRef>, ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(asset_yield::table
        .filter(asset_yield::chain_id.eq(chain_id))
        .select((asset_yield::asset_id_u64, asset_yield::venue))
        .load(&mut conn)
        .await?)
}
