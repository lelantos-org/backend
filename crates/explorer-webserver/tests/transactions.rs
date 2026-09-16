//! DB-backed tests for the classified-operation feed.
//!
//! A `Bundler` transaction lands several MASP operations under one hash, so the
//! classification has to scope each operation to its own log range. Only the
//! SQL knows those ranges, and a range that silently matches the whole
//! transaction drops operations rather than failing.
//!
//! Fixtures follow the log layout `Bundler.t.sol::test_execute_mixedBundle_logLayout`
//! pins. Only the logs these tables store are written; the gaps in `log_index`
//! stand in for `NullifierConsumed`, `NotePayload` and token `Transfer` logs.

use diesel::sql_types::{BigInt, Bytea, Integer, Nullable};
use diesel_async::RunQueryDsl;
use explorer_webserver::repositories::transactions;

const CHAIN: i64 = 1;
const ASSET: i64 = 7;
const BLOCK: i64 = 100;
const TS: i64 = 1_700_000_000;

const TABLES: &[&str] = &[
    "assets",
    "tree_advances",
    "asset_flows",
    "deposit_escrowed_events",
];

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    let (pool, guard) = test_support::fresh_pool(database::PoolCfg::indexer(), TABLES).await;
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO assets (chain_id, asset_id_u64, token, scale, decimals) \
         VALUES ($1, $2, '\\x11', 1, 0)",
    )
    .bind::<BigInt, _>(CHAIN)
    .bind::<BigInt, _>(ASSET)
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);
    (pool, guard)
}

fn tx(byte: u8) -> Vec<u8> {
    vec![byte; 32]
}

async fn root_advanced(pool: &database::DbPool, tx_hash: &[u8], log_index: i32) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO tree_advances \
           (chain_id, block_number, log_index, start_index, inserted, old_root, new_root, \
            tx_hash, block_ts) \
         VALUES ($1, $2, $3, 0, 2, '\\x00', '\\x00', $4, $5)",
    )
    .bind::<BigInt, _>(CHAIN)
    .bind::<BigInt, _>(BLOCK)
    .bind::<Integer, _>(log_index)
    .bind::<Bytea, _>(tx_hash)
    .bind::<BigInt, _>(TS)
    .execute(&mut conn)
    .await
    .unwrap();
}

/// A withdrawal's `AssetMoved(0, out)`.
async fn asset_moved_out(pool: &database::DbPool, tx_hash: &[u8], log_index: i32) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO asset_flows \
           (chain_id, block_number, log_index, asset_id_u64, token, in_amount, out_amount, \
            tx_hash, block_ts) \
         VALUES ($1, $2, $3, $4, '\\x11', 0, 5, $5, $6)",
    )
    .bind::<BigInt, _>(CHAIN)
    .bind::<BigInt, _>(BLOCK)
    .bind::<Integer, _>(log_index)
    .bind::<BigInt, _>(ASSET)
    .bind::<Bytea, _>(tx_hash)
    .bind::<BigInt, _>(TS)
    .execute(&mut conn)
    .await
    .unwrap();
}

/// A deposit escrowed in an earlier block and flushed by `flush_tx` at
/// `flushed_log_index`.
async fn flushed_deposit(
    pool: &database::DbPool,
    deposit_id: i64,
    flush_tx: &[u8],
    flushed_log_index: Option<i32>,
) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO deposit_escrowed_events \
           (chain_id, block_number, log_index, deposit_id, payer, recipient, public_asset_id, \
            public_in, fee_bps_at_submit, cm, cv_dep_x, cv_dep_y, rcv, aux, fee_asset_id, fee_in, \
            fee_cm, fee_cv_dep_x, fee_cv_dep_y, fee_rcv, fee_aux, submitted_at_block, tx_hash, \
            block_ts, \
            flushed_at_block, flushed_at_ts, flushed_tx_hash, flushed_log_index) \
         VALUES ($1, 90, $2, $2, '\\x01', '\\x02', $3, 10, 0, '\\x03', 0, 0, 0, '{}', 0, 0, '\\x04', \
                 0, 0, 0, '{}', 90, '\\x90', $4, $5, $6, $7, $8)",
    )
    .bind::<BigInt, _>(CHAIN)
    .bind::<BigInt, _>(deposit_id)
    .bind::<BigInt, _>(ASSET)
    .bind::<BigInt, _>(TS - 60)
    .bind::<BigInt, _>(BLOCK)
    .bind::<BigInt, _>(TS)
    .bind::<Bytea, _>(flush_tx)
    .bind::<Nullable<Integer>, _>(flushed_log_index)
    .execute(&mut conn)
    .await
    .unwrap();
}

/// `(tx byte, kind, log_index)` per row, in feed order.
async fn feed(pool: &database::DbPool) -> Vec<(u8, String, Option<i32>)> {
    transactions::recent(pool, Some(CHAIN), Some(0), None, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.tx_hash[0], r.kind, r.log_index))
        .collect()
}

async fn counts(pool: &database::DbPool) -> Vec<(String, i64)> {
    let mut rows: Vec<_> = transactions::kind_counts(pool, Some(CHAIN), 86_400, Some(0))
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.kind, r.count))
        .collect();
    rows.sort();
    rows
}

/// `[flushBatch, transfer, withdraw]` in one transaction, laid out as
/// `F R | NNNN R PPPPPP | NNNN R (Transfer) A PPPPPP`. Classifying by hash
/// alone saw an `AssetMoved` and a flush in the transaction and dropped the
/// transfer.
#[tokio::test]
async fn a_mixed_bundle_classifies_each_operation() {
    let (pool, _guard) = fresh_pool().await;
    let b = tx(0xb1);

    flushed_deposit(&pool, 1, &b, Some(0)).await;
    root_advanced(&pool, &b, 1).await;
    root_advanced(&pool, &b, 6).await;
    root_advanced(&pool, &b, 17).await;
    asset_moved_out(&pool, &b, 19).await;

    assert_eq!(
        feed(&pool).await,
        vec![
            (0xb1, "withdraw".to_string(), Some(19)),
            (0xb1, "transfer".to_string(), Some(6)),
            (0xb1, "deposit".to_string(), Some(0)),
        ]
    );
    assert_eq!(
        counts(&pool).await,
        vec![
            ("deposit".to_string(), 1),
            ("transfer".to_string(), 1),
            ("withdraw".to_string(), 1),
        ]
    );
}

/// Two operations of one kind under one hash are two rows, told apart by their
/// `log_index`.
#[tokio::test]
async fn two_transfers_in_one_bundle_are_two_rows() {
    let (pool, _guard) = fresh_pool().await;
    let b = tx(0xb2);

    root_advanced(&pool, &b, 4).await;
    root_advanced(&pool, &b, 15).await;

    assert_eq!(
        feed(&pool).await,
        vec![
            (0xb2, "transfer".to_string(), Some(15)),
            (0xb2, "transfer".to_string(), Some(4)),
        ]
    );
    assert_eq!(counts(&pool).await, vec![("transfer".to_string(), 2)]);
}

/// A flush recorded before `flushed_log_index` existed has no position, and
/// predates bundling, so its transaction is still excluded whole.
#[tokio::test]
async fn a_flush_without_a_position_is_never_a_transfer() {
    let (pool, _guard) = fresh_pool().await;
    let f = tx(0xf1);

    flushed_deposit(&pool, 1, &f, None).await;
    root_advanced(&pool, &f, 1).await;

    assert_eq!(feed(&pool).await, vec![(0xf1, "deposit".to_string(), None)]);
}

/// A legacy flush with no recorded position ranks after the operations in its
/// block that have one, instead of Postgres' default of NULLs first.
#[tokio::test]
async fn a_flush_without_a_position_sorts_last_in_its_block() {
    let (pool, _guard) = fresh_pool().await;
    let f = tx(0xf2);
    let w = tx(0xa1);

    flushed_deposit(&pool, 1, &f, None).await;
    root_advanced(&pool, &f, 1).await;
    root_advanced(&pool, &w, 5).await;
    asset_moved_out(&pool, &w, 7).await;

    assert_eq!(
        feed(&pool).await,
        vec![
            (0xa1, "withdraw".to_string(), Some(7)),
            (0xf2, "deposit".to_string(), None),
        ]
    );
}
