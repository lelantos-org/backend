//! DB-backed tests for the recorded index history the rate estimate reads,
//! and for the estimate's own write path.
//!
//! The SQL is where this can quietly go wrong: the writer copies rows across
//! tables and leans on a primary key to absorb repeats, and the reader measures
//! an elapsed time in the database rather than in Rust. None of that is visible
//! to the type checker, and a window query that silently returns nothing looks
//! exactly like a venue that cannot be measured.

use asset_registry::ApyEstimate;
use bigdecimal::BigDecimal;
use bigdecimal::FromPrimitive;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use protocol_webserver::repositories::{asset_yield, yield_samples};

const CHAIN: i64 = 1;
const ASSET: i64 = 7;
const DAY: i64 = 24 * 60 * 60;

/// Both tables the writer reads from and writes to.
const TABLES: &[&str] = &["asset_yield", "asset_yield_sample"];

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    test_support::fresh_pool(database::PoolCfg::indexer(), TABLES).await
}

/// The current reading, as protocol-indexer would leave it: `updated_at` set to
/// `now()` less `age_s`, so a test can place it relative to its samples.
async fn set_current(pool: &database::DbPool, index: u64, age_s: i64) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO asset_yield \
           (chain_id, asset_id_u64, venue, buffer_bps, perf_bps, halted, index_ray, \
            block_number, updated_at) \
         VALUES ($1, $2, '\\x11', 0, 0, false, $3, 100, now() - make_interval(secs => $4)) \
         ON CONFLICT (chain_id, asset_id_u64) DO UPDATE \
           SET index_ray = EXCLUDED.index_ray, updated_at = EXCLUDED.updated_at",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::BigInt, _>(ASSET)
    .bind::<diesel::sql_types::Numeric, _>(BigDecimal::from_u64(index).unwrap())
    .bind::<diesel::sql_types::BigInt, _>(age_s)
    .execute(&mut conn)
    .await
    .unwrap();
}

/// A sample as if recorded `age_s` ago.
async fn sample_at(pool: &database::DbPool, index: u64, age_s: i64) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO asset_yield_sample \
           (chain_id, asset_id_u64, observed_at, index_ray, block_number) \
         VALUES ($1, $2, now() - make_interval(secs => $3), $4, 1) \
         ON CONFLICT DO NOTHING",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::BigInt, _>(ASSET)
    .bind::<diesel::sql_types::BigInt, _>(age_s)
    .bind::<diesel::sql_types::Numeric, _>(BigDecimal::from_u64(index).unwrap())
    .execute(&mut conn)
    .await
    .unwrap();
}

/// A sample at an absolute instant.
///
/// Buckets are epoch divisions, so a fixture placed relative to `now()` can
/// straddle a boundary differently on every run. These tests pin the instant
/// instead, which makes the bucket a fact rather than a coincidence. Any epoch
/// in the past works: every one of them is far older than the coarsest tier.
async fn sample_at_epoch(pool: &database::DbPool, index: u64, epoch_s: i64) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO asset_yield_sample \
           (chain_id, asset_id_u64, observed_at, index_ray, block_number) \
         VALUES ($1, $2, to_timestamp($3), $4, 1) \
         ON CONFLICT DO NOTHING",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::BigInt, _>(ASSET)
    .bind::<diesel::sql_types::BigInt, _>(epoch_s)
    .bind::<diesel::sql_types::Numeric, _>(BigDecimal::from_u64(index).unwrap())
    .execute(&mut conn)
    .await
    .unwrap();
}

#[derive(QueryableByName)]
struct Epoch {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    epoch_s: i64,
}

/// Surviving samples, oldest first, as epoch seconds.
async fn survivors(pool: &database::DbPool) -> Vec<i64> {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "SELECT extract(epoch FROM observed_at)::BIGINT AS epoch_s \
           FROM asset_yield_sample ORDER BY observed_at",
    )
    .get_results::<Epoch>(&mut conn)
    .await
    .unwrap()
    .into_iter()
    .map(|e| e.epoch_s)
    .collect()
}

/// The window for the one asset these tests use, out of the chain's map.
async fn window_for(pool: &database::DbPool) -> Option<yield_samples::Sample> {
    let mut all = yield_samples::windows(pool, CHAIN, 2 * DAY, 14 * DAY)
        .await
        .unwrap();
    all.remove(&ASSET)
}

async fn count(pool: &database::DbPool) -> i64 {
    use database::schema::asset_yield_sample::dsl as s;
    let mut conn = pool.get().await.unwrap();
    s::asset_yield_sample
        .count()
        .get_result(&mut conn)
        .await
        .unwrap()
}

#[tokio::test]
async fn records_the_current_reading() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_000, 0).await;

    assert_eq!(yield_samples::record(&pool, CHAIN).await.unwrap(), 1);
    assert_eq!(count(&pool).await, 1);
}

/// Two passes inside one of the indexer's heartbeats see the same `updated_at`
/// and must store one row, not two.
///
/// Note this is the backstop, not the common case: the indexer refreshes
/// `updated_at` every 30 seconds whether or not the values moved, so in
/// production consecutive passes normally do see a new stamp and do store a new
/// row. That is the intended sampling cadence — see `record`.
#[tokio::test]
async fn two_passes_inside_one_heartbeat_store_one_row() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_000, 0).await;

    assert_eq!(yield_samples::record(&pool, CHAIN).await.unwrap(), 1);
    assert_eq!(yield_samples::record(&pool, CHAIN).await.unwrap(), 0);
    assert_eq!(count(&pool).await, 1);
}

/// An asset the indexer has never polled has no index to record. Storing a row
/// for it would put a null where the estimate expects a number.
#[tokio::test]
async fn an_unpolled_asset_is_not_recorded() {
    let (pool, _guard) = fresh_pool().await;
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO asset_yield (chain_id, asset_id_u64, venue, buffer_bps, perf_bps, halted) \
         VALUES ($1, $2, '\\x11', 0, 0, false)",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::BigInt, _>(ASSET)
    .execute(&mut conn)
    .await
    .unwrap();

    assert_eq!(yield_samples::record(&pool, CHAIN).await.unwrap(), 0);
}

#[tokio::test]
async fn measures_the_span_between_the_two_stamps() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_010, 0).await;
    sample_at(&pool, 1_000, 7 * DAY).await;

    let got = window_for(&pool)
        .await
        .expect("a sample a week old is inside the window");
    assert_eq!(got.index_ray, BigDecimal::from_u64(1_000).unwrap());
    // Within a second: both stamps are database clocks taken moments apart.
    assert!((got.elapsed_s - 7 * DAY).abs() <= 1, "{}", got.elapsed_s);
}

/// The floor exists so a rate is never annualized off a few hours. A history
/// that has not reached back far enough must report nothing rather than the
/// nearest thing it has.
#[tokio::test]
async fn a_history_shorter_than_the_floor_answers_nothing() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_010, 0).await;
    sample_at(&pool, 1_000, DAY).await;

    assert!(window_for(&pool).await.is_none());
}

/// The oldest inside the window, not the newest: a longer span is annualized by
/// a smaller exponent and magnifies less.
#[tokio::test]
async fn takes_the_oldest_sample_still_inside_the_window() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_030, 0).await;
    sample_at(&pool, 1_000, 20 * DAY).await; // too old
    sample_at(&pool, 1_010, 10 * DAY).await; // the one to use
    sample_at(&pool, 1_020, 3 * DAY).await;

    let got = window_for(&pool).await.unwrap();
    assert_eq!(got.index_ray, BigDecimal::from_u64(1_010).unwrap());
}

/// The current reading is `asset_yield`'s, so a sample newer than it — the clock
/// having moved, or a stale poll — must not produce a negative span.
#[tokio::test]
async fn a_sample_newer_than_the_reading_is_not_a_window() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_010, 10 * DAY).await;
    sample_at(&pool, 1_000, 0).await;

    assert!(window_for(&pool).await.is_none());
}

/// A 12-hour bucket boundary, and the epochs either side of it. Any multiple of
/// the coarsest bucket works; this one is in 2023, so every fixture built on it
/// is older than the coarsest tier no matter when the test runs.
const BUCKET_12H: i64 = 12 * 60 * 60;
const BOUNDARY: i64 = 1_700_000_000 / BUCKET_12H * BUCKET_12H;

/// The whole point of the coarse tiers: one sample survives each bucket, and it
/// is the earliest, so the series stays anchored at the oldest reading rather
/// than drifting forward every pass.
#[tokio::test]
async fn thin_keeps_the_earliest_sample_in_a_bucket() {
    let (pool, _guard) = fresh_pool().await;
    sample_at_epoch(&pool, 1_000, BOUNDARY).await;
    sample_at_epoch(&pool, 1_001, BOUNDARY + 100).await;
    sample_at_epoch(&pool, 1_002, BOUNDARY + 200).await;

    yield_samples::thin(&pool, CHAIN).await.unwrap();

    assert_eq!(survivors(&pool).await, vec![BOUNDARY]);
}

/// Bucketing is an epoch division, so two readings a second apart across a
/// boundary belong to different buckets and both survive. This is what makes
/// the retention independent of the session time zone — `date_trunc` on a
/// `TIMESTAMPTZ` would place this boundary differently per connection.
#[tokio::test]
async fn thin_splits_buckets_on_an_epoch_boundary() {
    let (pool, _guard) = fresh_pool().await;
    sample_at_epoch(&pool, 1_000, BOUNDARY - 1).await;
    sample_at_epoch(&pool, 1_001, BOUNDARY).await;

    yield_samples::thin(&pool, CHAIN).await.unwrap();

    assert_eq!(survivors(&pool).await, vec![BOUNDARY - 1, BOUNDARY]);
}

/// Recent samples are the ones a fresh note's basis interpolates between, so
/// the finest tier leaves them all in place.
#[tokio::test]
async fn thin_leaves_the_newest_samples_at_full_resolution() {
    let (pool, _guard) = fresh_pool().await;
    for age in [600, 1_800, 3_600, 7_200] {
        sample_at(&pool, 1_000 + age as u64, age).await;
    }

    assert_eq!(yield_samples::thin(&pool, CHAIN).await.unwrap(), 0);
    assert_eq!(count(&pool).await, 4);
}

/// Beyond the coarsest tier, half-hourly readings collapse to one per twelve
/// hours — the retention that bounds the published series.
#[tokio::test]
async fn thin_collapses_old_samples_to_the_coarsest_bucket() {
    let (pool, _guard) = fresh_pool().await;
    // Two full 12-hour buckets sampled every 30 minutes.
    for i in 0..48 {
        sample_at_epoch(&pool, 1_000 + i as u64, BOUNDARY + i * 1_800).await;
    }

    yield_samples::thin(&pool, CHAIN).await.unwrap();

    assert_eq!(
        survivors(&pool).await,
        vec![BOUNDARY, BOUNDARY + BUCKET_12H],
        "one per bucket, each the earliest"
    );
}

/// An asset stops being sampled when its venue is unbound. Thinning must not
/// erase what it already had, or the basis for every note it backs is lost.
#[tokio::test]
async fn thin_never_removes_an_assets_only_sample() {
    let (pool, _guard) = fresh_pool().await;
    sample_at(&pool, 1_000, 400 * DAY).await;

    assert_eq!(yield_samples::thin(&pool, CHAIN).await.unwrap(), 0);
    assert_eq!(count(&pool).await, 1);
}

/// Retention is shared with the rate estimate, which reads the oldest sample in
/// `[2d, 14d]`. That band is thinned to hourly, so it must still answer.
#[tokio::test]
async fn thin_keeps_a_sample_the_apy_window_can_still_reach() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_030, 0).await;
    // Every half hour across the window, as the 30-minute writer would leave it.
    for i in 0..(12 * 24 * 2) {
        sample_at(&pool, 1_000 + i as u64, 12 * DAY - i * 1_800).await;
    }

    yield_samples::thin(&pool, CHAIN).await.unwrap();

    assert!(
        window_for(&pool).await.is_some(),
        "the estimate still needs a sample between two and fourteen days old"
    );
}

/// One asset's history must not be thinned against another's readings.
#[tokio::test]
async fn thin_buckets_each_asset_separately() {
    let (pool, _guard) = fresh_pool().await;
    let other = ASSET + 1;
    let mut conn = pool.get().await.unwrap();
    for (asset, epoch) in [(ASSET, BOUNDARY), (other, BOUNDARY + 100)] {
        diesel::sql_query(
            "INSERT INTO asset_yield_sample \
               (chain_id, asset_id_u64, observed_at, index_ray, block_number) \
             VALUES ($1, $2, to_timestamp($3), 1000, 1)",
        )
        .bind::<diesel::sql_types::BigInt, _>(CHAIN)
        .bind::<diesel::sql_types::BigInt, _>(asset)
        .bind::<diesel::sql_types::BigInt, _>(epoch)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    drop(conn);

    assert_eq!(yield_samples::thin(&pool, CHAIN).await.unwrap(), 0);
    assert_eq!(count(&pool).await, 2);
}

/// The query serves a chain, so it must key each asset to its own sample rather
/// than letting the oldest row on the chain answer for every asset.
#[tokio::test]
async fn keeps_each_asset_to_its_own_sample() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_030, 0).await;
    sample_at(&pool, 1_010, 10 * DAY).await;

    let other = ASSET + 1;
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO asset_yield \
           (chain_id, asset_id_u64, venue, buffer_bps, perf_bps, halted, index_ray, \
            block_number, updated_at) \
         VALUES ($1, $2, '\\x22', 0, 0, false, 2000, 100, now())",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::BigInt, _>(other)
    .execute(&mut conn)
    .await
    .unwrap();
    diesel::sql_query(
        "INSERT INTO asset_yield_sample \
           (chain_id, asset_id_u64, observed_at, index_ray, block_number) \
         VALUES ($1, $2, now() - make_interval(secs => $3), 1900, 1)",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::BigInt, _>(other)
    .bind::<diesel::sql_types::BigInt, _>(5 * DAY)
    .execute(&mut conn)
    .await
    .unwrap();

    let all = yield_samples::windows(&pool, CHAIN, 2 * DAY, 14 * DAY)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[&ASSET].index_ray, BigDecimal::from_u64(1_010).unwrap());
    assert_eq!(all[&other].index_ray, BigDecimal::from_u64(1_900).unwrap());
}

/// The estimate's columns, read back raw.
#[derive(QueryableByName)]
struct StoredApy {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    apy_bps: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    apy_window_s: Option<i64>,
    /// Only its presence is asserted: the value is `now()` on the writer's clock.
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
    apy_measured_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}

async fn stored_apy(pool: &database::DbPool, asset: i64) -> StoredApy {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "SELECT apy_bps, apy_window_s, apy_measured_at FROM asset_yield \
          WHERE chain_id = $1 AND asset_id_u64 = $2",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::BigInt, _>(asset)
    .get_result::<StoredApy>(&mut conn)
    .await
    .unwrap()
}

#[tokio::test]
async fn an_estimate_is_stored_against_its_asset() {
    let (pool, _g) = fresh_pool().await;
    set_current(&pool, 1_000, 0).await;

    let est = ApyEstimate {
        bps: 512,
        window_s: 7 * DAY,
    };
    assert!(
        asset_yield::store_estimate(&pool, CHAIN, ASSET, est)
            .await
            .unwrap(),
        "the asset has a yield row, so the update must land"
    );

    let stored = stored_apy(&pool, ASSET).await;
    assert_eq!(stored.apy_bps, Some(512));
    assert_eq!(stored.apy_window_s, Some(7 * DAY));
    assert!(
        stored.apy_measured_at.is_some(),
        "readers age the figure against this, so it cannot be left null"
    );
}

/// A venue can lose. `SMALLINT` would also have held this one — the column is
/// `INTEGER` for the positive end, where the estimate is capped at 1,000,000 bps.
#[tokio::test]
async fn a_rate_outside_smallint_round_trips() {
    let (pool, _g) = fresh_pool().await;
    set_current(&pool, 1_000, 0).await;

    for bps in [-10_000, 999_999] {
        asset_yield::store_estimate(
            &pool,
            CHAIN,
            ASSET,
            ApyEstimate {
                bps,
                window_s: 7 * DAY,
            },
        )
        .await
        .unwrap();
        assert_eq!(stored_apy(&pool, ASSET).await.apy_bps, Some(bps));
    }
}

/// An estimate only exists for an asset already bound to a venue, and that
/// binding is what creates the row. A miss means the asset is not yield-bearing,
/// so nothing is written rather than a binding being invented.
#[tokio::test]
async fn an_asset_with_no_yield_row_is_not_given_one() {
    let (pool, _g) = fresh_pool().await;

    let stored = asset_yield::store_estimate(
        &pool,
        CHAIN,
        ASSET,
        ApyEstimate {
            bps: 1,
            window_s: 7 * DAY,
        },
    )
    .await
    .unwrap();
    assert!(!stored, "no row to update");

    let mut conn = pool.get().await.unwrap();
    let n: i64 = diesel::sql_query("SELECT count(*) AS count FROM asset_yield")
        .get_result::<Count>(&mut conn)
        .await
        .unwrap()
        .count;
    assert_eq!(n, 0, "no asset_yield row may be created by storing a rate");
}
