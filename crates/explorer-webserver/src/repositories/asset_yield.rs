use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Bool, Nullable, Numeric, SmallInt, Text};
use diesel_async::RunQueryDsl;

/// One yield-bearing asset: its venue binding, and the last state polled from
/// `yieldState`.
///
/// Having a row in `asset_yield` is what makes an asset yield-bearing —
/// `YieldAssetAdded` creates the binding and the contract emits nothing that
/// undoes it — so this needs no `is_yield` predicate and no filter for one.
///
/// The two halves arrive by different mechanisms and are absent for different
/// reasons. `venue`, `buffer_bps`, `perf_bps` and `halted` come from logs and
/// are always present. Everything from `total_normalized` down is polled, and is
/// `NULL` until the first poll for that asset lands — a newly bound asset, or one
/// whose venue was unreachable on every pass so far.
#[derive(Debug, Clone, QueryableByName)]
pub struct YieldRow {
    #[diesel(sql_type = BigInt)]
    pub chain_id: i64,
    #[diesel(sql_type = BigInt)]
    pub asset_id_u64: i64,
    /// Lowercase hex without a `0x` prefix, as in `asset_locked`.
    #[diesel(sql_type = Text)]
    pub token_hex: String,
    /// ERC20 decimals, or `NULL` while the indexer has not resolved it.
    #[diesel(sql_type = Nullable<SmallInt>)]
    pub decimals: Option<i16>,
    #[diesel(sql_type = Nullable<Text>)]
    pub symbol: Option<String>,
    /// The venue this asset's custody earns in, lowercase hex without `0x`.
    #[diesel(sql_type = Text)]
    pub venue_hex: String,
    #[diesel(sql_type = SmallInt)]
    pub buffer_bps: i16,
    #[diesel(sql_type = SmallInt)]
    pub perf_bps: i16,
    #[diesel(sql_type = Bool)]
    pub halted: bool,
    /// Normalized units owed to note holders. Together with
    /// `accrued_fee_normalized` this is the supply the conversion divides by.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub total_normalized: Option<BigDecimal>,
    /// The treasury's minted-but-unswept normalized units.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub accrued_fee_normalized: Option<BigDecimal>,
    /// Underlying the pool holds for this asset outside the venue, in base
    /// units.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub idle: Option<BigDecimal>,
    /// Venue position plus idle, in base units: everything backing the asset,
    /// and the numerator of the conversion.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub gross: Option<BigDecimal>,
    /// The conversion rate scaled by RAY (1e27). Display only — see
    /// `services::asset_yield::fee_underlying` for why no amount is rebuilt from
    /// it.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub index_ray: Option<BigDecimal>,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub block_number: Option<i64>,
    /// Poll time as a unix timestamp. Seconds rather than a timestamp encoding
    /// so freshness is the same integer field as `last_ts` on the other
    /// endpoints.
    #[diesel(sql_type = Nullable<BigInt>)]
    pub updated_at: Option<i64>,
}

/// Every yield-bearing asset, newest state first read from the poller's row.
///
/// Joined to `assets` rather than reported bare: an amount in base units is
/// meaningless without `decimals`, and the registry row is guaranteed present
/// because `MASP.addYieldAsset` goes through the add-only registry, so the
/// binding cannot exist for an asset the registry has never seen.
///
/// Ordered by the registry id, not by size: `gross` is `NULL` for an unpolled
/// asset and base units of different tokens are not comparable, so any
/// size ordering would be arbitrary between assets and unstable between
/// requests.
pub async fn list(pool: &DbPool, chain_id: Option<i64>) -> AppResult<Vec<YieldRow>> {
    let mut conn = super::conn(pool).await?;
    sql_query(
        "SELECT y.chain_id AS chain_id, \
                y.asset_id_u64 AS asset_id_u64, \
                encode(a.token, 'hex') AS token_hex, \
                a.decimals AS decimals, \
                a.symbol AS symbol, \
                encode(y.venue, 'hex') AS venue_hex, \
                y.buffer_bps AS buffer_bps, \
                y.perf_bps AS perf_bps, \
                y.halted AS halted, \
                y.total_normalized AS total_normalized, \
                y.accrued_fee_normalized AS accrued_fee_normalized, \
                y.idle AS idle, \
                y.gross AS gross, \
                y.index_ray AS index_ray, \
                y.block_number AS block_number, \
                EXTRACT(EPOCH FROM y.updated_at)::BIGINT AS updated_at \
         FROM asset_yield y \
         JOIN assets a \
           ON a.chain_id = y.chain_id \
          AND a.asset_id_u64 = y.asset_id_u64 \
         WHERE ($1::BIGINT IS NULL OR y.chain_id = $1) \
         ORDER BY y.chain_id, y.asset_id_u64",
    )
    .bind::<Nullable<BigInt>, _>(chain_id)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
