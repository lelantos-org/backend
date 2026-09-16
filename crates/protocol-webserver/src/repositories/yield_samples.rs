//! A written-down history of the pool's yield index.
//!
//! `asset_yield` carries one row per asset and is overwritten on every indexer
//! pass, so a week-old index exists nowhere: recovering it means a historical
//! `eth_call` against archive state, and public RPCs prune within hours. This
//! table is the way around that — every reading is copied as it goes past, and a
//! week later the comparison is a query.
//!
//! Nothing here reads the chain. The samples are copied from `asset_yield`,
//! which protocol-indexer keeps current, so a pass costs one statement per chain
//! and no RPC at all.
//!
//! Written by whichever replica holds this chain's measurement lock; see
//! `crate::handlers::worker::venue_apy`.

use crate::domain::error::AppResult;
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::sql_types::BigInt;
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
/// rather than the common case: protocol-indexer rewrites `updated_at` on a
/// heartbeat whether or not the values moved, so consecutive passes
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
    .map_err(super::db_err)
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
/// `asset_registry::list_for_chains` is: asking per asset would cost a checkout
/// each, out of a pool this service also serves every request from. An asset
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
    .map_err(super::db_err)
}

/// One recorded reading, as the published history carries it.
#[derive(Debug, QueryableByName)]
pub struct HistoryRow {
    #[diesel(sql_type = BigInt)]
    pub asset_id_u64: i64,
    #[diesel(sql_type = BigInt)]
    pub block_number: i64,
    #[diesel(sql_type = diesel::sql_types::Numeric)]
    pub index_ray: BigDecimal,
}

/// Every retained reading on `chain_id`, oldest block first within each asset.
///
/// Serves note cost basis: a client interpolates between the two readings
/// bracketing a note's block. That makes `block_number` the axis, so rows
/// without one are useless here and excluded — they predate the poller writing
/// it.
///
/// `DISTINCT ON` because protocol-indexer re-stamps `updated_at` on a heartbeat
/// whether or not the chain moved, so a quiet chain records several readings at
/// one block. They agree — index and block come from the same read — but a
/// series with a repeated x has no single answer to interpolate through.
pub async fn history(pool: &DbPool, chain_id: i64) -> AppResult<Vec<HistoryRow>> {
    let mut conn = super::conn(pool).await?;
    sql_query(
        "SELECT DISTINCT ON (asset_id_u64, block_number) \
                asset_id_u64, block_number, index_ray \
           FROM asset_yield_sample \
          WHERE chain_id = $1 AND block_number IS NOT NULL \
          ORDER BY asset_id_u64, block_number, observed_at",
    )
    .bind::<BigInt, _>(chain_id)
    .get_results::<HistoryRow>(&mut conn)
    .await
    .map_err(super::db_err)
}

const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

/// Retention tiers as `(older_than_s, bucket_s)`, finest first.
///
/// Applied in order and cumulatively: a reading is thinned by every tier whose
/// age it exceeds, so it ends at the coarsest applicable bucket. A year-old
/// sample passes through all three and survives at one per twelve hours; one
/// three days old is touched only by the first and survives hourly; one inside
/// two days is touched by none.
///
/// Sized against what reads this. The finest band covers the notes whose basis
/// is interpolated most often, and the rate estimate's `[2d, 14d]` window falls
/// in the hourly band. The coarse tail bounds a published series at roughly 730
/// rows per asset per year; the error it costs a cost basis is the index growth
/// across one bucket, which at 5% APY is under 0.007%.
const TIERS: &[(i64, i64)] = &[
    (2 * DAY, HOUR),
    (30 * DAY, 6 * HOUR),
    (180 * DAY, 12 * HOUR),
];

/// Thin the history to `TIERS`, keeping the **earliest** reading in each
/// bucket.
///
/// Earliest rather than latest so the series stays anchored: re-running must not
/// walk a bucket's surviving sample forward, or a basis interpolated through it
/// would drift with every pass.
///
/// Replaces a flat age cutoff. Cost basis reaches back as far as the oldest
/// unspent note, which is unbounded, so a window that merely bounded growth
/// would silently stop answering for long-held notes.
///
/// Buckets are epoch divisions rather than `date_trunc`, which on a
/// `TIMESTAMPTZ` truncates in the *session* time zone — so which rows this
/// deleted would otherwise depend on the connection that ran it.
///
/// Run on the same tick as [`record`]. Returns how many samples were dropped.
pub async fn thin(pool: &DbPool, chain_id: i64) -> AppResult<usize> {
    let mut conn = super::conn(pool).await?;
    let mut removed = 0;

    for &(older_than_s, bucket_s) in TIERS {
        // A row is deleted when an *older* row shares its bucket, which leaves
        // exactly the earliest of each. `now()` is the database's, so the
        // cutoff is not the caller's clock.
        removed += sql_query(
            "DELETE FROM asset_yield_sample d \
              USING asset_yield_sample k \
              WHERE d.chain_id = $1 \
                AND k.chain_id = d.chain_id \
                AND k.asset_id_u64 = d.asset_id_u64 \
                AND d.observed_at < now() - make_interval(secs => $2) \
                AND k.observed_at < d.observed_at \
                AND floor(extract(epoch FROM k.observed_at) / $3) \
                  = floor(extract(epoch FROM d.observed_at) / $3)",
        )
        .bind::<BigInt, _>(chain_id)
        .bind::<BigInt, _>(older_than_s)
        .bind::<BigInt, _>(bucket_s)
        .execute(&mut conn)
        .await
        .map_err(super::db_err)?;
    }
    Ok(removed)
}
