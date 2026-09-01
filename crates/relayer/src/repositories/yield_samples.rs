//! A written-down history of the pool's yield index.
//!
//! `asset_yield` carries one row per asset and is overwritten on every indexer
//! pass, so a week-old index exists nowhere: recovering it means a historical
//! `eth_call` against archive state, and public RPCs prune within hours. This
//! table is the way around that — every reading is copied as it goes past, and a
//! week later the comparison is a query.
//!
//! Nothing here reads the chain. The samples are copied from `asset_yield`,
//! which explorer-indexer keeps current, so a pass costs one statement per chain
//! and no RPC at all.

use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::IntoSql;
use diesel::pg::expression::extensions::IntervalDsl;
use diesel::prelude::*;
use diesel::sql_types::BigInt;
use diesel::sql_types::Timestamptz;
use diesel::{QueryableByName, sql_query};
use diesel_async::RunQueryDsl;
use std::collections::HashMap;

/// Copy every current reading on `chain_id` into the history.
///
/// `INSERT … SELECT` rather than a read followed by a write: the values, and the
/// timestamp they are stamped with, come from the same statement that reads
/// them, so a sample can never carry one asset's index under another's clock.
///
/// `observed_at` is `asset_yield.updated_at` — when the indexer last *confirmed*
/// the value against the chain, not when this ran. The elapsed time between two
/// samples is the denominator of a rate, and the writer's own clock would fold
/// its polling jitter into it.
///
/// Rows with nothing to record are skipped by the `WHERE`.
///
/// The primary key absorbs a reading already stored, but that is a backstop
/// rather than the common case: explorer-indexer rewrites `updated_at` on a
/// 30-second heartbeat whether or not the values moved, so consecutive passes
/// almost always see a fresh stamp and store a fresh row. What the key actually
/// guards is a second writer, or a pass that runs twice inside one heartbeat.
///
/// Returns how many samples were new.
pub async fn record(pool: &DbPool, chain_id: i64) -> AppResult<usize> {
    let mut conn = super::conn(pool).await?;
    sql_query(
        "INSERT INTO asset_yield_sample \
           (chain_id, asset_id_u64, observed_at, index_ray, block_number) \
         SELECT chain_id, asset_id_u64, updated_at, index_ray, block_number \
           FROM asset_yield \
          WHERE chain_id = $1 AND index_ray IS NOT NULL AND updated_at IS NOT NULL \
         ON CONFLICT DO NOTHING",
    )
    .bind::<BigInt, _>(chain_id)
    .execute(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}

/// The oldest sample of one asset inside the measurement window.
#[derive(Debug, QueryableByName)]
pub struct Sample {
    #[diesel(sql_type = BigInt)]
    pub asset_id_u64: i64,
    /// Seconds between the sample and the current reading, measured between the
    /// two `updated_at` stamps rather than against the clock of whoever asked.
    #[diesel(sql_type = BigInt)]
    pub elapsed_s: i64,
    #[diesel(sql_type = diesel::sql_types::Numeric)]
    pub index_ray: BigDecimal,
}

/// The oldest sample of each asset on `chain_id` that is at least `min_age_s`
/// old and at most `max_age_s` old, keyed by asset id.
///
/// One statement and one pooled connection for the whole chain, as
/// `assets::list_for_chains` is: asking per asset costs a checkout each, and the
/// relayer's pool has four connections shared with the submission path. An asset
/// whose history does not yet reach back far enough is absent from the map.
///
/// The oldest inside the window rather than the newest: a longer span is a
/// better measurement, since the exponent that annualizes it is smaller and
/// magnifies less. The upper bound is what keeps the span from growing without
/// limit as the history does — a rate measured over four months describes a
/// venue that may no longer exist.
///
/// Joined against `asset_yield` rather than taking the current index as an
/// argument, so both ends of the comparison come from the same statement and
/// cannot be a poll apart.
pub async fn windows(
    pool: &DbPool,
    chain_id: i64,
    min_age_s: i64,
    max_age_s: i64,
) -> AppResult<HashMap<i64, Sample>> {
    let mut conn = super::conn(pool).await?;
    // `DISTINCT ON` with the matching `ORDER BY` is Postgres' argmin: one row per
    // asset, and the `ASC` picks the oldest sample still inside the window.
    sql_query(
        "SELECT DISTINCT ON (s.asset_id_u64) \
                s.asset_id_u64, \
                EXTRACT(EPOCH FROM (y.updated_at - s.observed_at))::BIGINT AS elapsed_s, \
                s.index_ray \
           FROM asset_yield y \
           JOIN asset_yield_sample s \
             ON s.chain_id = y.chain_id AND s.asset_id_u64 = y.asset_id_u64 \
          WHERE y.chain_id = $1 \
            AND y.updated_at IS NOT NULL \
            AND s.observed_at <= y.updated_at - make_interval(secs => $2) \
            AND s.observed_at >= y.updated_at - make_interval(secs => $3) \
          ORDER BY s.asset_id_u64, s.observed_at ASC",
    )
    .bind::<BigInt, _>(chain_id)
    .bind::<BigInt, _>(min_age_s)
    .bind::<BigInt, _>(max_age_s)
    .get_results::<Sample>(&mut conn)
    .await
    .map(|rows| rows.into_iter().map(|r| (r.asset_id_u64, r)).collect())
    .map_err(|e| AppError::Db(e.to_string()))
}

/// Drop samples older than the widest window anything will ask for.
///
/// Without this the table grows for the life of the deployment to hold readings
/// no query can reach. Run on the same tick as [`record`], so the history stays
/// bounded without a job of its own.
pub async fn prune(pool: &DbPool, chain_id: i64, older_than_s: i64) -> AppResult<usize> {
    use database::schema::asset_yield_sample::dsl as s;
    let mut conn = super::conn(pool).await?;
    // The DSL rather than a string, as the sibling repositories delete: the two
    // statements above earn their raw SQL with an `INSERT … SELECT` and a
    // cross-table `EXTRACT`, and this one does not. `now()` is still the
    // database's, so the cutoff is not the caller's clock.
    diesel::delete(
        s::asset_yield_sample.filter(
            s::chain_id.eq(chain_id).and(
                s::observed_at
                    .lt(diesel::dsl::now.into_sql::<Timestamptz>()
                        - (older_than_s as f64).seconds()),
            ),
        ),
    )
    .execute(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
