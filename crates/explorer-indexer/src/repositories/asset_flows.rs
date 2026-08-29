use crate::error::ExplorerIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::asset_flows;
use diesel::prelude::*;
use diesel::sql_query;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = asset_flows)]
pub struct NewAssetFlow {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub in_amount: BigDecimal,
    pub out_amount: BigDecimal,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
    /// The same movement in circuit units, as published by the SNARK.
    ///
    /// `Option` because rows written before the contract emitted these fields
    /// carry NULL. Aggregations over denominations must skip NULL rather than
    /// read it as a zero-valued denomination.
    pub public_in: Option<BigDecimal>,
    pub public_out: Option<BigDecimal>,
}

pub async fn refresh_hourly_mv(pool: &DbPool) -> Result<(), ExplorerIndexerError> {
    refresh(pool, "asset_flows_hourly").await
}

/// All-time per-(chain, asset) escrow totals. Refreshed on the same trigger as
/// the hourly view: both derive from `asset_flows`, so a tick that changes one
/// changes the other.
pub async fn refresh_locked_mv(pool: &DbPool) -> Result<(), ExplorerIndexerError> {
    refresh(pool, "asset_locked").await
}

/// `CONCURRENTLY`, so readers keep serving the previous contents while the
/// refresh runs; each of these views backs a polling dashboard.
///
/// `view` is a compile-time literal at every call site. It is interpolated
/// because a view name cannot be a bind parameter.
async fn refresh(pool: &DbPool, view: &'static str) -> Result<(), ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    sql_query(format!("REFRESH MATERIALIZED VIEW CONCURRENTLY {view}"))
        .execute(&mut conn)
        .await?;
    Ok(())
}

pub async fn insert(pool: &DbPool, row: NewAssetFlow) -> Result<usize, ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(diesel::insert_into(asset_flows::table)
        .values(&row)
        .on_conflict((
            asset_flows::chain_id,
            asset_flows::block_number,
            asset_flows::log_index,
        ))
        .do_nothing()
        .execute(&mut conn)
        .await?)
}
