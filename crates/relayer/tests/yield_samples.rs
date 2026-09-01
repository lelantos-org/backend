//! DB-backed tests for the recorded index history the rate estimate reads.
//!
//! The SQL is where this can quietly go wrong: the writer copies rows across
//! tables and leans on a primary key to absorb repeats, and the reader measures
//! an elapsed time in the database rather than in Rust. None of that is visible
//! to the type checker, and a window query that silently returns nothing looks
//! exactly like a venue that cannot be measured.

use bigdecimal::BigDecimal;
use bigdecimal::FromPrimitive;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use relayer::repositories::yield_samples;

const CHAIN: i64 = 1;
const ASSET: i64 = 7;
const DAY: i64 = 24 * 60 * 60;

/// Both tables the writer reads from and writes to.
const TABLES: &[&str] = &["asset_yield", "asset_yield_sample"];

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    test_support::fresh_pool(database::PoolCfg::indexer(), TABLES).await
}

/// The current reading, as explorer-indexer would leave it: `updated_at` set to
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

#[tokio::test]
async fn pruning_keeps_what_the_window_can_still_reach() {
    let (pool, _guard) = fresh_pool().await;
    set_current(&pool, 1_030, 0).await;
    sample_at(&pool, 1_000, 20 * DAY).await;
    sample_at(&pool, 1_010, 10 * DAY).await;

    assert_eq!(
        yield_samples::prune(&pool, CHAIN, 14 * DAY).await.unwrap(),
        1
    );
    assert_eq!(count(&pool).await, 1);
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
