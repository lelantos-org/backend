use crate::adapters::numeric::bigdecimal_to_u256;
use crate::domain::error::{AppError, AppResult};
use crate::domain::units::{Rate, Scale};
use alloy::primitives::Address;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::{asset_yield, assets};
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
    /// Per-leg fee rates, `NULL` until an `AssetFeeSet` has been indexed.
    ///
    /// There is no pool-wide rate to fall back to, so `NULL` means unknown and
    /// a consumer must decline to quote rather than assume zero. Both are
    /// bounded by `MAX_FEE_BPS` (2000) on chain, so `SMALLINT` cannot hold a
    /// value a `uint16` could not.
    pub deposit_bps: Option<i16>,
    pub withdraw_bps: Option<i16>,
    /// The venue this asset's custody earns in, or `NULL` for a plain asset.
    ///
    /// Present iff the asset has an `asset_yield` row, which the contract
    /// creates once and can never undo.
    pub venue: Option<Vec<u8>>,
    /// Venue position plus idle, and the units outstanding against it.
    ///
    /// Both `NULL` until the indexer's first poll lands. A unit of this asset is
    /// worth `gross / supply` rather than `scale`, so a consumer that has the
    /// venue but not these two knows the asset is yield-bearing and that it
    /// cannot yet price it — which is different from pricing it at `scale` and
    /// being wrong by however much the venue has earned.
    pub gross: Option<BigDecimal>,
    pub total_normalized: Option<BigDecimal>,
    pub accrued_fee_normalized: Option<BigDecimal>,
    pub halted: Option<bool>,
    pub index_ray: Option<BigDecimal>,
}

impl AssetRow {
    /// The MASP asset id, stored as `i64` because Postgres has no unsigned
    /// integer. It is a `u64` everywhere else, and this is where the conversion
    /// belongs.
    pub fn asset_id(&self) -> u64 {
        self.asset_id_u64 as u64
    }

    /// This asset's circuit-to-base rate, or `None` if it cannot be priced yet.
    ///
    /// `None` means a yield asset whose index has not been polled — never a
    /// plain asset, which prices at `scale` forever. Callers must not substitute
    /// `scale` for a missing index: `scale` is not a conservative default but
    /// wrong by whatever the venue has earned, in the direction that quotes too
    /// many units and then credits too few.
    pub fn rate(&self, scale: Scale) -> Option<Rate> {
        if self.venue.is_none() {
            return Some(Rate::plain(scale));
        }
        let gross = bigdecimal_to_u256(self.gross.as_ref()?).ok()?;
        let total = bigdecimal_to_u256(self.total_normalized.as_ref()?).ok()?;
        let fee = bigdecimal_to_u256(self.accrued_fee_normalized.as_ref()?).ok()?;
        Some(Rate::yielding(scale, gross, total + fee))
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
        .left_join(
            asset_yield::table.on(asset_yield::chain_id
                .eq(assets::chain_id)
                .and(asset_yield::asset_id_u64.eq(assets::asset_id_u64))),
        )
        .filter(assets::chain_id.eq(chain_id))
        .order(assets::asset_id_u64.asc())
        .select((
            assets::asset_id_u64,
            assets::token,
            assets::scale,
            assets::decimals,
            assets::symbol,
            assets::deposit_bps,
            assets::withdraw_bps,
            asset_yield::venue.nullable(),
            asset_yield::gross.nullable(),
            asset_yield::total_normalized.nullable(),
            asset_yield::accrued_fee_normalized.nullable(),
            asset_yield::halted.nullable(),
            asset_yield::index_ray.nullable(),
        ))
        .load::<AssetRow>(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}

/// Every registered asset on each of `chain_ids`, ordered by `(chain_id,
/// asset_id_u64)`.
///
/// One statement and one pooled connection for the whole set. Calling
/// [`list_for_chain`] per chain costs a checkout each, and the relayer pool has
/// four connections.
pub async fn list_for_chains(pool: &DbPool, chain_ids: &[i64]) -> AppResult<Vec<(i64, AssetRow)>> {
    if chain_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = super::conn(pool).await?;
    assets::table
        .left_join(
            asset_yield::table.on(asset_yield::chain_id
                .eq(assets::chain_id)
                .and(asset_yield::asset_id_u64.eq(assets::asset_id_u64))),
        )
        .filter(assets::chain_id.eq_any(chain_ids))
        .order((assets::chain_id.asc(), assets::asset_id_u64.asc()))
        .select((
            assets::chain_id,
            (
                assets::asset_id_u64,
                assets::token,
                assets::scale,
                assets::decimals,
                assets::symbol,
                assets::deposit_bps,
                assets::withdraw_bps,
                asset_yield::venue.nullable(),
                asset_yield::gross.nullable(),
                asset_yield::total_normalized.nullable(),
                asset_yield::accrued_fee_normalized.nullable(),
                asset_yield::halted.nullable(),
                asset_yield::index_ray.nullable(),
            ),
        ))
        .load::<(i64, AssetRow)>(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
