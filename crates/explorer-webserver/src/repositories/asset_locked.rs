use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Nullable, Numeric, SmallInt, Text};
use diesel_async::RunQueryDsl;

/// One asset's all-time escrow totals on one chain.
#[derive(Debug, Clone, QueryableByName)]
pub struct LockedRow {
    #[diesel(sql_type = BigInt)]
    pub chain_id: i64,
    #[diesel(sql_type = BigInt)]
    pub asset_id_u64: i64,
    /// Lowercase hex, no `0x` — the token identity used for price lookup.
    #[diesel(sql_type = Text)]
    pub token_hex: String,
    /// ERC20 decimals, or `NULL` if the indexer has not resolved it yet.
    #[diesel(sql_type = Nullable<SmallInt>)]
    pub decimals: Option<i16>,
    #[diesel(sql_type = Nullable<Text>)]
    pub symbol: Option<String>,
    /// Deposits, in token base units, over all history.
    #[diesel(sql_type = Numeric)]
    pub in_base: BigDecimal,
    /// Withdrawals, same unit. Locked is `in_base - out_base`.
    #[diesel(sql_type = Numeric)]
    pub out_base: BigDecimal,
    /// Newest flow that contributed, so a caller can age the figure.
    #[diesel(sql_type = BigInt)]
    pub last_ts: i64,
}

/// Every asset that has ever moved, with its running totals.
///
/// Assets with no flows at all are absent: `asset_locked` aggregates
/// `asset_flows`, so a registered-but-untouched asset has no row. That is the
/// honest shape — it has never held anything — and the caller reports the chains
/// rather than padding out a registry.
///
/// No cross-asset arithmetic here, for the same reason `flow_buckets` does none:
/// base units of different tokens are not addable, so the totals stay per asset
/// and `decimals` rides along for the caller to convert with.
pub async fn totals(pool: &DbPool, chain_id: Option<i64>) -> AppResult<Vec<LockedRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    sql_query(
        "SELECT l.chain_id AS chain_id, \
                l.asset_id_u64 AS asset_id_u64, \
                encode(l.token, 'hex') AS token_hex, \
                a.decimals AS decimals, \
                a.symbol AS symbol, \
                l.in_base AS in_base, \
                l.out_base AS out_base, \
                l.last_ts AS last_ts \
         FROM asset_locked l \
         JOIN assets a \
           ON a.chain_id = l.chain_id \
          AND a.asset_id_u64 = l.asset_id_u64 \
         WHERE ($1::BIGINT IS NULL OR l.chain_id = $1) \
         ORDER BY l.chain_id, l.asset_id_u64",
    )
    .bind::<Nullable<BigInt>, _>(chain_id)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
