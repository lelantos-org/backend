use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::assets;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// One registered asset, as `/chains` publishes it.
///
/// The table is written by explorer-indexer; the relayer only reads it. That
/// crosses a service boundary through the database, which is deliberate: the
/// registry a wallet boots from should be one request, and the relayer is
/// already the service that enumerates chains.
#[derive(Debug, Clone, Queryable)]
pub struct AssetRow {
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub scale: BigDecimal,
    /// `NULL` until the indexer has read `decimals()` over RPC.
    pub decimals: Option<i16>,
    /// `NULL` until the indexer has read `symbol()`, or when the token does
    /// not implement it.
    pub symbol: Option<String>,
}

/// Every registered asset on `chain_id`, lowest id first.
pub async fn list_for_chain(pool: &DbPool, chain_id: i64) -> AppResult<Vec<AssetRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    assets::table
        .filter(assets::chain_id.eq(chain_id))
        .order(assets::asset_id_u64.asc())
        .select((
            assets::asset_id_u64,
            assets::token,
            assets::scale,
            assets::decimals,
            assets::symbol,
        ))
        .load::<AssetRow>(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
