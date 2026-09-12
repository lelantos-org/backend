use crate::domain::error::ProtocolIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::asset_yield;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// One asset's venue binding, from `YieldAssetAdded`.
///
/// Creating this row is what makes an asset yield-bearing; the contract has no
/// event that undoes it. Idempotent, so a cursor rewind can replay the binding.
#[derive(Debug, Clone, Insertable, AsChangeset)]
#[diesel(table_name = asset_yield)]
pub struct UpsertBinding {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub venue: Vec<u8>,
    pub buffer_bps: i16,
    pub perf_bps: i16,
}

/// One `YieldParamsSet`, as [`set_params_batch`] applies it.
#[derive(Debug, Clone, Copy)]
pub struct SetParams {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub buffer_bps: i16,
    pub perf_bps: i16,
}

/// One `HaltedSet`, as [`set_halted_batch`] applies it.
#[derive(Debug, Clone, Copy)]
pub struct SetHalted {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub halted: bool,
}

/// Bind a tick's venues over one pooled connection, in event order.
///
/// One statement per row rather than one multi-row `ON CONFLICT DO UPDATE`,
/// which Postgres rejects when a single statement would touch the same key
/// twice; see [`super::assets::upsert_batch`]. The caller deduplicates anyway —
/// `services::consume::yields::YieldPlan::push_binding` keeps the last binding
/// per asset — so a window carrying two `YieldAssetAdded` events for one asset
/// writes the later one.
pub async fn upsert_binding_batch(
    pool: &DbPool,
    rows: &[UpsertBinding],
) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        diesel::insert_into(asset_yield::table)
            .values(row)
            .on_conflict((asset_yield::chain_id, asset_yield::asset_id_u64))
            .do_update()
            .set(row)
            .execute(&mut conn)
            .await?;
    }
    Ok(())
}

/// Apply a tick's parameter changes over one pooled connection, in event order.
pub async fn set_params_batch(
    pool: &DbPool,
    rows: &[SetParams],
) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        diesel::update(asset_yield::table.find((row.chain_id, row.asset_id_u64)))
            .set((
                asset_yield::buffer_bps.eq(row.buffer_bps),
                asset_yield::perf_bps.eq(row.perf_bps),
            ))
            .execute(&mut conn)
            .await?;
    }
    Ok(())
}

/// Apply a tick's halt flags over one pooled connection, in event order.
pub async fn set_halted_batch(
    pool: &DbPool,
    rows: &[SetHalted],
) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        diesel::update(asset_yield::table.find((row.chain_id, row.asset_id_u64)))
            .set(asset_yield::halted.eq(row.halted))
            .execute(&mut conn)
            .await?;
    }
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
) -> Result<(), ProtocolIndexerError> {
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
) -> Result<Vec<YieldAssetRef>, ProtocolIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(asset_yield::table
        .filter(asset_yield::chain_id.eq(chain_id))
        .select((asset_yield::asset_id_u64, asset_yield::venue))
        .load(&mut conn)
        .await?)
}

/// Yield assets on `chain_id` whose vault name has not been read yet.
pub async fn missing_vault_name(
    pool: &DbPool,
    chain_id: i64,
    limit: i64,
) -> Result<Vec<YieldAssetRef>, ProtocolIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(asset_yield::table
        .filter(asset_yield::chain_id.eq(chain_id))
        .filter(asset_yield::vault_name.is_null())
        .order(asset_yield::asset_id_u64.asc())
        .limit(limit)
        .select((asset_yield::asset_id_u64, asset_yield::venue))
        .load(&mut conn)
        .await?)
}

pub async fn set_vault_name(
    pool: &DbPool,
    chain_id: i64,
    asset_id_u64: i64,
    name: &str,
) -> Result<(), ProtocolIndexerError> {
    let mut conn = super::conn(pool).await?;
    diesel::update(asset_yield::table.find((chain_id, asset_id_u64)))
        .set(asset_yield::vault_name.eq(name))
        .execute(&mut conn)
        .await?;
    Ok(())
}
