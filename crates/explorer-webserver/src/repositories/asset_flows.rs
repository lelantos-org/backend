use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Nullable, Numeric, SmallInt, Text};
use diesel_async::RunQueryDsl;

/// One bucket's flow for one asset. Kept per-asset rather than pre-summed
/// because USD conversion needs each token's own price and decimals.
#[derive(Debug, Clone, QueryableByName)]
pub struct FlowBucketRow {
    #[diesel(sql_type = BigInt)]
    pub ts: i64,
    #[diesel(sql_type = BigInt)]
    pub chain_id: i64,
    /// Lowercase hex, no `0x` — the token identity used for price lookup.
    #[diesel(sql_type = Text)]
    pub token_hex: String,
    /// ERC20 decimals, or `NULL` if the indexer has not resolved it yet.
    /// Without it neither a token amount nor a USD figure can be produced.
    #[diesel(sql_type = Nullable<SmallInt>)]
    pub decimals: Option<i16>,
    /// Token base units. Every conversion starts here: whole tokens are
    /// `in_base / 10^decimals`, and USD is that times the spot price.
    #[diesel(sql_type = Numeric)]
    pub in_base: BigDecimal,
    #[diesel(sql_type = Numeric)]
    pub out_base: BigDecimal,
}

/// Aggregate flows into `bucket_sec` buckets, per asset, in **base units**.
///
/// Deliberately no cross-asset arithmetic here. Base units of different tokens
/// are not addable, and neither are circuit units: `scale` sizes the value for
/// the circuit (`baseUnits / scale` must fit `uint48`), it does not normalise
/// decimals. An 18-decimal token at `scale = 1e10` has 1e8 circuit units per
/// whole token while a 6-decimal token at `scale = 1` has 1e6, so a sum of
/// circuit units is off by up to 100x per asset mixed in.
///
/// Grouping therefore stops at `(bucket, chain, token)`, and `decimals` rides
/// along so the caller can convert each asset on its own before combining
/// anything — which is only valid in USD.
pub async fn flow_buckets(
    pool: &DbPool,
    chain_id: Option<i64>,
    asset_id_u64: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Vec<FlowBucketRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    sql_query(
        "SELECT (f.ts_hour / $1) * $1 AS ts, \
                f.chain_id AS chain_id, \
                encode(a.token, 'hex') AS token_hex, \
                a.decimals AS decimals, \
                SUM(f.in_amount)::NUMERIC(78, 0)  AS in_base, \
                SUM(f.out_amount)::NUMERIC(78, 0) AS out_base \
         FROM asset_flows_hourly f \
         JOIN assets a \
           ON a.chain_id = f.chain_id \
          AND a.asset_id_u64 = f.asset_id_u64 \
         WHERE ($2::BIGINT IS NULL OR f.chain_id = $2) \
           AND ($3::BIGINT IS NULL OR f.asset_id_u64 = $3) \
           AND ($4::BIGINT IS NULL OR f.ts_hour >= $4) \
         GROUP BY 1, 2, 3, 4 \
         ORDER BY 1",
    )
    .bind::<BigInt, _>(bucket_sec)
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<Nullable<BigInt>, _>(asset_id_u64)
    .bind::<Nullable<BigInt>, _>(since_ts)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
