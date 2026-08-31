//! Withdrawal anonymity sets, grouped by denomination.
//!
//! A withdrawal's `publicOut` is the one integer it publishes, so its anonymity
//! set is every other withdrawal that published the same integer. See
//! `docs/src/guide/denominations.md`: the set is *actual, not potential* — it is
//! however many users withdrew that denomination, not however many could have.
//!
//! Grouping is on the raw circuit-unit integer, never on a converted amount. A
//! denomination is fixed while the yield index moves what it is worth, so two
//! withdrawals of the same denomination are one set even when their whole-token
//! values differ.

use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Nullable, Text};
use diesel_async::RunQueryDsl;

/// One denomination's cohort on one chain, for one asset.
#[derive(Debug, Clone, QueryableByName)]
pub struct DenominationRow {
    #[diesel(sql_type = BigInt)]
    pub chain_id: i64,
    #[diesel(sql_type = BigInt)]
    pub asset_id_u64: i64,
    /// The published circuit value, as a decimal string.
    ///
    /// Text rather than `BigInt`: the column is `NUMERIC(20,0)` holding a
    /// `uint64`, whose maximum (1.8e19) exceeds `i64::MAX` (9.2e18). A
    /// denomination near the top of the range would overflow the bind.
    #[diesel(sql_type = Text)]
    pub public_out: String,
    /// How many withdrawals published this denomination. This is the k in
    /// k-anonymity, over all history.
    #[diesel(sql_type = BigInt)]
    pub count: i64,
    /// How many of those fell inside the caller's lookback. A subset of
    /// `count`, computed alongside it rather than by a second query so the two
    /// cannot describe different sets of rows.
    #[diesel(sql_type = BigInt)]
    pub recent_count: i64,
    #[diesel(sql_type = BigInt)]
    pub first_ts: i64,
    #[diesel(sql_type = BigInt)]
    pub last_ts: i64,
}

/// Cohort sizes per `(chain, asset, denomination)`, over all history.
///
/// No time filter, by design: partitioning an anonymity set by time shrinks it,
/// and a denomination's cover is every withdrawal of that size the pool has ever
/// seen. A caller that windowed this would report a smaller k than the user
/// actually has.
///
/// `recent_count` is an aggregate over the same grouped rows rather than a
/// second query: a separate statement could land either side of a new
/// withdrawal and report a recency count larger than the total it belongs to.
/// `block_ts` is not part of `asset_flows_public_out_idx`, but the rows are
/// already being read to be grouped, so the filter costs a comparison on tuples
/// the scan reaches anyway — no new index, no migration.
///
/// The grouping names `f.public_out`, the numeric column, rather than the
/// `::TEXT` projection or its ordinal. Grouping on the cast makes the raw column
/// unavailable to `ORDER BY` — Postgres rejects the statement outright — and
/// ordering on the text would sort a ladder lexicographically, putting 1000
/// before 200. Casting a grouped column in the select list is free.
///
/// `public_out IS NOT NULL` is a filter, never a `COALESCE`. NULL means the row
/// was indexed before the contract emitted the field, so its denomination is
/// unknown; counting it as a zero denomination would invent a cohort that does
/// not exist. The partial index `asset_flows_public_out_covering_idx` covers
/// exactly this predicate, and `INCLUDE (block_ts)` carries the column the three
/// timestamp aggregates below need, so the whole thing runs index-only rather
/// than fetching every grouped row from the heap for one field.
pub async fn denominations(
    pool: &DbPool,
    chain_id: Option<i64>,
    asset_id_u64: Option<i64>,
    limit: i64,
    recent_from_ts: i64,
) -> AppResult<Vec<DenominationRow>> {
    let mut conn = super::conn(pool).await?;
    sql_query(
        "SELECT f.chain_id AS chain_id, \
                f.asset_id_u64 AS asset_id_u64, \
                f.public_out::TEXT AS public_out, \
                COUNT(*)::BIGINT AS count, \
                COUNT(*) FILTER (WHERE f.block_ts >= $3)::BIGINT AS recent_count, \
                MIN(f.block_ts) AS first_ts, \
                MAX(f.block_ts) AS last_ts \
         FROM asset_flows f \
         WHERE f.public_out IS NOT NULL AND f.public_out > 0 \
           AND ($1::BIGINT IS NULL OR f.chain_id = $1) \
           AND ($2::BIGINT IS NULL OR f.asset_id_u64 = $2) \
         GROUP BY f.chain_id, f.asset_id_u64, f.public_out \
         ORDER BY f.chain_id, f.asset_id_u64, f.public_out \
         LIMIT $4",
    )
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<Nullable<BigInt>, _>(asset_id_u64)
    .bind::<BigInt, _>(recent_from_ts)
    .bind::<BigInt, _>(limit)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
