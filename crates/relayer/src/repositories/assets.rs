use crate::domain::error::{AppError, AppResult};
use alloy::primitives::Address;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::assets;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// One registered asset, as `/chains` publishes it.
///
/// The table is written by explorer-indexer and read by the relayer, crossing a
/// service boundary through the database: the registry a wallet boots from should
/// be one request, and the relayer already enumerates chains.
#[derive(Debug, Clone, Queryable)]
pub struct AssetRow {
    pub asset_id_u64: i64,
    pub token: Vec<u8>,
    pub scale: BigDecimal,
    /// `NULL` until the indexer has read `decimals()` over RPC.
    pub decimals: Option<i16>,
    /// `NULL` until the indexer has read `symbol()`, or permanently for a token
    /// that does not implement it.
    pub symbol: Option<String>,
}

impl AssetRow {
    /// The MASP asset id, stored as `i64` because Postgres has no unsigned
    /// integer. It is a `u64` everywhere else, and this is where the conversion
    /// belongs.
    pub fn asset_id(&self) -> u64 {
        self.asset_id_u64 as u64
    }

    /// The ERC-20 address, or `None` if the column does not hold 20 bytes.
    ///
    /// Fallible rather than `Address::from_slice`, which panics on any other
    /// width. The column is written by another service, so a row of the wrong shape
    /// is a data problem to report rather than one that stops the submit path.
    pub fn token_address(&self) -> Option<Address> {
        Address::try_from(self.token.as_slice()).ok()
    }
}

/// Every registered asset on `chain_id`, lowest id first.
pub async fn list_for_chain(pool: &DbPool, chain_id: i64) -> AppResult<Vec<AssetRow>> {
    let mut conn = super::conn(pool).await?;
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
