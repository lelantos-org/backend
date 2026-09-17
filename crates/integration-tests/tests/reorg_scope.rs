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
    "gov_proposals",
    "gov_votes",
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

/// An escrow older than the fork survives, but its flush and cancel inside the
/// fork are undone, so the deposit reads as pending again until the replay
/// re-marks what the new chain still has.
#[tokio::test]
async fn the_protocol_half_unmarks_flushes_and_cancels_inside_the_fork() {
    let (pool, _guard) = fresh_pool().await;
    let mut conn = pool.get().await.unwrap();
    for (deposit_id, flushed_at, canceled_at) in [
        (1i64, Some(FORK_AT), None),
        (2, None, Some(FORK_AT + 1)),
        (3, Some(FORK_AT - 1), None),
    ] {
        diesel::sql_query(
            "INSERT INTO deposit_escrowed_events (chain_id, block_number, log_index, deposit_id, \
               payer, recipient, public_asset_id, public_in, fee_bps_at_submit, cm, cv_dep_x, \
               cv_dep_y, rcv, aux, fee_asset_id, fee_in, fee_cm, fee_cv_dep_x, fee_cv_dep_y, \
               fee_rcv, fee_aux, submitted_at_block, flushed_at_block, flushed_at_ts, \
               flushed_tx_hash, \
               flushed_log_index, canceled_at_block, tx_hash, block_ts) \
             VALUES ($1, $2, $3, $3, '\\x00', '\\x00', 1, 0, 0, '\\x00', 0, 0, 0, '{}', 0, 0, '\\x00', \
               0, 0, 0, '{}', $2, $4, $4, CASE WHEN $4 IS NULL THEN NULL ELSE '\\x02'::bytea END, \
               CASE WHEN $4 IS NULL THEN NULL ELSE 0 END, $5, '\\x02', 0)",
        )
        .bind::<diesel::sql_types::BigInt, _>(CHAIN)
        .bind::<diesel::sql_types::BigInt, _>(FORK_AT - 10)
        .bind::<diesel::sql_types::Integer, _>(deposit_id as i32)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::BigInt>, _>(flushed_at)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::BigInt>, _>(canceled_at)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    drop(conn);
    record_reorg(&pool).await;

    database::reorg::apply_pending(&pool, Owner::Protocol, CHAIN)
        .await
        .unwrap();

    #[derive(QueryableByName)]
    struct Marks {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        flushed_at_block: Option<i64>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
        flushed_log_index: Option<i32>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        canceled_at_block: Option<i64>,
    }
    let mut conn = pool.get().await.unwrap();
    let marks: Vec<Marks> = diesel::sql_query(
        "SELECT flushed_at_block, flushed_log_index, canceled_at_block \
         FROM deposit_escrowed_events ORDER BY log_index",
    )
    .load(&mut conn)
    .await
    .unwrap();
    let marks: Vec<_> = marks
        .iter()
        .map(|m| (m.flushed_at_block, m.flushed_log_index, m.canceled_at_block))
        .collect();

    assert_eq!(
        marks,
        [
            (None, None, None),
            (None, None, None),
            (Some(FORK_AT - 1), Some(0), None),
        ],
        "marks inside the fork are cleared; one before it stays"
    );
}

/// Governance follows the deposit ledger: proposals and votes inside the fork
/// go, and an older proposal's lifecycle marks inside the fork are undone —
/// `eta` with its queue mark — so the replay re-marks only what the new chain
/// still has. Nothing is touched for the explorer.
#[tokio::test]
async fn the_protocol_half_retracts_governance_inside_the_fork() {
    let (pool, _guard) = fresh_pool().await;
    let mut conn = pool.get().await.unwrap();
    // (proposal id, created at, queued at, executed at, canceled at)
    for (id, created, queued, executed, canceled) in [
        (1i64, FORK_AT, None, None, None),
        (2, FORK_AT - 10, Some(FORK_AT), Some(FORK_AT + 1), None),
        (3, FORK_AT - 10, Some(FORK_AT - 5), Some(FORK_AT - 1), None),
        (4, FORK_AT - 10, None, None, Some(FORK_AT + 2)),
    ] {
        diesel::sql_query(
            "INSERT INTO gov_proposals (chain_id, proposal_id, proposer, targets, call_values, \
               signatures, calldatas, description, vote_start, vote_end, quorum_vote_deadline, \
               block_number, log_index, tx_hash, block_ts, queued_at_block, eta, \
               executed_at_block, canceled_at_block) \
             VALUES ($1, $2, '\\x00', '{}', '{}', '{}', '{}', '', 0, 0, 0, $3, 0, '\\x02', 0, \
               $4, CASE WHEN $4 IS NULL THEN NULL ELSE 999 END, $5, $6)",
        )
        .bind::<diesel::sql_types::BigInt, _>(CHAIN)
        .bind::<diesel::sql_types::BigInt, _>(id)
        .bind::<diesel::sql_types::BigInt, _>(created)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::BigInt>, _>(queued)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::BigInt>, _>(executed)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::BigInt>, _>(canceled)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    for (voter, block) in [(1i32, FORK_AT - 1), (2, FORK_AT)] {
        diesel::sql_query(
            "INSERT INTO gov_votes (chain_id, proposal_id, voter, support, weight, reason, \
               block_number, log_index, tx_hash, block_ts) \
             VALUES ($1, 2, int4send($2), 1, 10, '', $3, 0, '\\x02', 0)",
        )
        .bind::<diesel::sql_types::BigInt, _>(CHAIN)
        .bind::<diesel::sql_types::Integer, _>(voter)
        .bind::<diesel::sql_types::BigInt, _>(block)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    drop(conn);
    seed_derived(&pool).await;
    record_reorg(&pool).await;

    database::reorg::apply_pending(&pool, Owner::Protocol, CHAIN)
        .await
        .unwrap();

    #[derive(QueryableByName)]
    struct Marks {
        #[diesel(sql_type = diesel::sql_types::Numeric)]
        proposal_id: bigdecimal::BigDecimal,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        queued_at_block: Option<i64>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        eta: Option<i64>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        executed_at_block: Option<i64>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
        canceled_at_block: Option<i64>,
    }
    let mut conn = pool.get().await.unwrap();
    let marks: Vec<Marks> = diesel::sql_query(
        "SELECT proposal_id, queued_at_block, eta, executed_at_block, canceled_at_block \
         FROM gov_proposals ORDER BY proposal_id",
    )
    .load(&mut conn)
    .await
    .unwrap();
    drop(conn);
    let marks: Vec<_> = marks
        .iter()
        .map(|m| {
            (
                m.proposal_id.to_string(),
                m.queued_at_block,
                m.eta,
                m.executed_at_block,
                m.canceled_at_block,
            )
        })
        .collect();
    assert_eq!(
        marks,
        [
            ("2".to_string(), None, None, None, None),
            (
                "3".to_string(),
                Some(FORK_AT - 5),
                Some(999),
                Some(FORK_AT - 1),
                None
            ),
            ("4".to_string(), None, None, None, None),
        ],
        "the proposal created in the fork is gone; marks inside it are cleared"
    );
    assert_eq!(
        count(&pool, "gov_votes").await,
        1,
        "the vote inside the fork goes"
    );
    assert_eq!(
        count(&pool, "asset_flows").await,
        1,
        "explorer rows untouched"
    );
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
