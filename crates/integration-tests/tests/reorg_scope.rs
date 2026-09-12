//! A retraction deletes only the caller's own derived tables.
//!
//! Three consumers now read `raw_events`: fmd-indexer, protocol-indexer and
//! explorer-indexer. Retraction used to delete every derived table regardless of
//! who asked, which worked only because each consumer independently reached the
//! same reorg record and replayed — leaving a window where one consumer's rows
//! were gone with only another's cursor rewound.
//!
//! With three consumers that window matters: `tree_advances` and
//! `deposit_escrowed_events` are on the relayer's write path, so an explorer
//! retraction emptying them would stall the flush pipeline until protocol-indexer
//! happened to catch up.

use database::reorg::Owner;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

const CHAIN: i64 = 1;
const FORK_AT: i64 = 100;

const TABLES: &[&str] = &[
    "chain_reorgs",
    "consumer_cursors",
    "notes",
    "spent_nullifiers",
    "tree_state",
    "tree_advances",
    "deposit_escrowed_events",
    "asset_flows",
    "yield_fee_events",
];

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    test_support::fresh_pool(database::PoolCfg::relayer(), TABLES).await
}

/// One row per owner, all at the forked block, so a retraction that is too wide
/// shows up as somebody else's row going missing.
async fn seed_derived(pool: &database::DbPool) {
    let mut conn = pool.get().await.unwrap();
    for sql in [
        "INSERT INTO tree_advances (chain_id, block_number, log_index, start_index, \
           inserted, old_root, new_root, tx_hash, block_ts) \
         VALUES ($1, $2, 0, 0, 1, '\\x00', '\\x01', '\\x02', 0)",
        "INSERT INTO asset_flows (chain_id, block_number, log_index, asset_id_u64, token, \
           in_amount, out_amount, tx_hash, block_ts) \
         VALUES ($1, $2, 0, 1, '\\x11', 0, 0, '\\x02', 0)",
        "INSERT INTO yield_fee_events (chain_id, asset_id_u64, block_number, block_ts, \
           tx_hash, log_index, kind, units) \
         VALUES ($1, $2, $2, 0, '\\x02', 0, 1, 0)",
    ] {
        diesel::sql_query(sql)
            .bind::<diesel::sql_types::BigInt, _>(CHAIN)
            .bind::<diesel::sql_types::BigInt, _>(FORK_AT)
            .execute(&mut conn)
            .await
            .unwrap();
    }
}

async fn count(pool: &database::DbPool, table: &str) -> i64 {
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        count: i64,
    }
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(format!("SELECT count(*) AS count FROM {table}"))
        .get_result::<Count>(&mut conn)
        .await
        .unwrap()
        .count
}

async fn record_reorg(pool: &database::DbPool) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query("INSERT INTO chain_reorgs (chain_id, rewind_to) VALUES ($1, $2)")
        .bind::<diesel::sql_types::BigInt, _>(CHAIN)
        .bind::<diesel::sql_types::BigInt, _>(FORK_AT)
        .execute(&mut conn)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_retraction_deletes_only_the_callers_tables() {
    let (pool, _guard) = fresh_pool().await;
    seed_derived(&pool).await;
    record_reorg(&pool).await;

    // The explorer half retracts. Its own two tables go.
    database::reorg::apply_pending(&pool, Owner::Explorer, CHAIN)
        .await
        .unwrap();

    assert_eq!(count(&pool, "asset_flows").await, 0);
    assert_eq!(count(&pool, "yield_fee_events").await, 0);
    assert_eq!(
        count(&pool, "tree_advances").await,
        1,
        "the relayer reads `tree_advances`; an explorer retraction must not empty it"
    );
}

/// The counterpart: the protocol half retracts its ledgers and leaves analytics
/// alone, so a fork does not blank the explorer's charts for rows it still holds
/// the cursor position for.
#[tokio::test]
async fn the_protocol_half_leaves_the_explorers_tables_alone() {
    let (pool, _guard) = fresh_pool().await;
    seed_derived(&pool).await;
    record_reorg(&pool).await;

    database::reorg::apply_pending(&pool, Owner::Protocol, CHAIN)
        .await
        .unwrap();

    assert_eq!(count(&pool, "tree_advances").await, 0);
    assert_eq!(count(&pool, "asset_flows").await, 1);
    assert_eq!(count(&pool, "yield_fee_events").await, 1);
}

/// Each owner has its own cursor row and its own `last_reorg_id`, so one
/// consumer applying a reorg must not mark it applied for the others.
#[tokio::test]
async fn applying_a_reorg_rewinds_only_the_callers_cursor() {
    let (pool, _guard) = fresh_pool().await;
    seed_derived(&pool).await;
    record_reorg(&pool).await;

    assert_eq!(
        database::reorg::apply_pending(&pool, Owner::Explorer, CHAIN)
            .await
            .unwrap(),
        1
    );
    // Still pending for the other two: each rediscovers it from its own row.
    for owner in [Owner::Protocol, Owner::Fmd] {
        assert_eq!(
            database::reorg::apply_pending(&pool, owner, CHAIN)
                .await
                .unwrap(),
            1,
            "{owner:?} must still see the reorg it has not applied"
        );
    }
}

/// The cursor name and the retracted table set come from one enum, so a consumer
/// cannot commit under one name while retracting another's rows.
#[test]
fn each_owner_has_its_own_cursor_name() {
    let names = [Owner::Fmd, Owner::Protocol, Owner::Explorer].map(Owner::cursor_name);
    let unique: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "{names:?}");
}
