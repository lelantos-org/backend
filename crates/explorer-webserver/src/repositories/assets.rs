use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::assets;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = assets)]
pub struct AssetRow {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub scale: BigDecimal,
    /// `NULL` until the indexer has read `decimals()` over RPC: unknown, not 18.
    pub decimals: Option<i16>,
    /// `NULL` until the indexer has read `symbol()` over RPC, or permanently for
    /// tokens that do not implement it.
    pub symbol: Option<String>,
    /// Protocol fee rates, in bps, `NULL` until an `AssetFeeSet` is indexed.
    ///
    /// Per asset and per leg — the pool has no global rate — so `NULL` means
    /// unknown and must not be read as zero. `SMALLINT` is wide enough because
    /// the contract caps both at `MAX_FEE_BPS` (2000).
    pub deposit_bps: Option<i16>,
    pub withdraw_bps: Option<i16>,
}

pub async fn list(pool: &DbPool, chain_id: Option<i64>) -> AppResult<Vec<AssetRow>> {
    let mut conn = super::conn(pool).await?;
    let mut q = assets::table.into_boxed();
    if let Some(c) = chain_id {
        q = q.filter(assets::chain_id.eq(c));
    }
    q.order((assets::chain_id.asc(), assets::asset_id_u64.asc()))
        .select(AssetRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
