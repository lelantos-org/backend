//! Fixture-replay tests for the governor's events: encode them as a node would
//! report them, file them in `raw_events`, run one consume tick and read back
//! `gov_proposals` and `gov_votes`.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::eth::Log;
use alloy::sol_types::SolEvent;
use bigdecimal::BigDecimal;
use chain_types::abi::{
    ProposalCanceled, ProposalCreated, ProposalExecuted, ProposalQueued,
    ProposalQuorumVoteDeadline, VoteCast, VoteCastWithParams,
};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use protocol_indexer::services::consume::{ConsumeCtx, RefreshGate, tick_chain};
use shared::entities::EventKind;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use test_support::fixtures::{build_log_from, insert_chain_state, insert_log};

const CHAIN: i64 = 1;
const TS: u64 = 1_700_000_000;

const TABLES: &[&str] = &[
    "raw_events",
    "chain_state",
    "chain_reorgs",
    "consumer_cursors",
    "gov_proposals",
    "gov_votes",
];

fn governor() -> Address {
    Address::repeat_byte(0x60)
}

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    test_support::fresh_pool(database::PoolCfg::indexer(), TABLES).await
}

fn ctx(pool: database::DbPool, governor: Option<Address>) -> ConsumeCtx {
    ConsumeCtx {
        pool,
        token_meta: Arc::new(HashMap::new()),
        refresh: Arc::new(RefreshGate::new()),
        governors: Arc::new(governor.map(|g| (CHAIN, g)).into_iter().collect()),
    }
}

/// A log at `block`, `log_idx`, emitted by the governor.
fn gov_log<E: SolEvent>(ev: &E, block: u64, tx_byte: u8, log_idx: u64) -> Log {
    build_log_from(
        governor(),
        ev.encode_log_data(),
        block,
        TS + block,
        tx_byte,
        log_idx,
    )
}

fn created(id: u64) -> ProposalCreated {
    ProposalCreated {
        proposalId: U256::from(id),
        proposer: Address::repeat_byte(0x01),
        targets: vec![Address::repeat_byte(0x02), Address::repeat_byte(0x03)],
        values: vec![U256::ZERO, U256::from(7u64)],
        signatures: vec![String::new(), String::new()],
        calldatas: vec![vec![0xab, 0xcd].into(), vec![].into()],
        voteStart: U256::from(TS + 1),
        voteEnd: U256::from(TS + 301),
        description: "# Burn fees\n\nBody".into(),
    }
}

fn vote(voter: u8, id: u64, support: u8, weight: u64) -> VoteCast {
    VoteCast {
        voter: Address::repeat_byte(voter),
        proposalId: U256::from(id),
        support,
        weight: U256::from(weight),
        reason: format!("r{voter}"),
    }
}

async fn file(pool: &database::DbPool, log: &Log, kind: EventKind) {
    insert_log(pool, CHAIN, log, kind).await;
}

#[derive(QueryableByName, Debug, PartialEq)]
struct Tally {
    #[diesel(sql_type = diesel::sql_types::SmallInt)]
    support: i16,
    #[diesel(sql_type = diesel::sql_types::Numeric)]
    weight: BigDecimal,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    votes: i64,
}

async fn tallies(pool: &database::DbPool, id: u64) -> Vec<Tally> {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "SELECT support, SUM(weight) AS weight, COUNT(*) AS votes FROM gov_votes \
         WHERE chain_id = $1 AND proposal_id = $2 GROUP BY support ORDER BY support",
    )
    .bind::<diesel::sql_types::BigInt, _>(CHAIN)
    .bind::<diesel::sql_types::Numeric, _>(BigDecimal::from(id))
    .load(&mut conn)
    .await
    .unwrap()
}

async fn count(pool: &database::DbPool, table: &str) -> i64 {
    #[derive(QueryableByName)]
    struct C {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(format!("SELECT count(*) AS n FROM {table}"))
        .get_result::<C>(&mut conn)
        .await
        .unwrap()
        .n
}

/// Seed one proposal's whole life. `ProposalQuorumVoteDeadline` shares the
/// creating transaction, so it lands in the same window and must still find
/// the row the insert creates.
async fn seed_lifecycle(pool: &database::DbPool) {
    file(
        pool,
        &gov_log(&created(42), 100, 0x10, 0),
        EventKind::ProposalCreated,
    )
    .await;
    let deadline = ProposalQuorumVoteDeadline {
        proposalId: U256::from(42u64),
        quorumVoteDeadline: U256::from(TS + 241),
    };
    file(
        pool,
        &gov_log(&deadline, 100, 0x10, 1),
        EventKind::ProposalQuorumVoteDeadline,
    )
    .await;
    file(
        pool,
        &gov_log(&vote(0xa1, 42, 1, 300), 102, 0x11, 0),
        EventKind::VoteCast,
    )
    .await;
    file(
        pool,
        &gov_log(&vote(0xa2, 42, 1, 200), 102, 0x12, 1),
        EventKind::VoteCast,
    )
    .await;
    file(
        pool,
        &gov_log(&vote(0xa3, 42, 0, 50), 103, 0x13, 0),
        EventKind::VoteCast,
    )
    .await;
    let with_params = VoteCastWithParams {
        voter: Address::repeat_byte(0xa4),
        proposalId: U256::from(42u64),
        support: 2,
        weight: U256::from(5u64),
        reason: String::new(),
        params: vec![0x01, 0x02].into(),
    };
    file(
        pool,
        &gov_log(&with_params, 104, 0x14, 0),
        EventKind::VoteCastWithParams,
    )
    .await;
    let queued = ProposalQueued {
        proposalId: U256::from(42u64),
        etaSeconds: U256::from(TS + 400),
    };
    file(
        pool,
        &gov_log(&queued, 110, 0x15, 0),
        EventKind::ProposalQueued,
    )
    .await;
    let executed = ProposalExecuted {
        proposalId: U256::from(42u64),
    };
    file(
        pool,
        &gov_log(&executed, 120, 0x16, 2),
        EventKind::ProposalExecuted,
    )
    .await;
}

#[tokio::test]
async fn a_proposal_lifecycle_lands_with_tallies_and_marks() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN).await;
    seed_lifecycle(&pool).await;

    let _ = tick_chain(&ctx(pool.clone(), Some(governor())), CHAIN, 500)
        .await
        .unwrap();

    use database::schema::gov_proposals as g;
    let mut conn = pool.get().await.unwrap();
    #[allow(clippy::type_complexity)]
    let row: (
        BigDecimal,
        Vec<u8>,
        Vec<Vec<u8>>,
        Vec<BigDecimal>,
        Vec<Vec<u8>>,
        String,
        i64,
        i64,
        Option<i64>,
        i64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) = g::table
        .filter(g::chain_id.eq(CHAIN))
        .select((
            g::proposal_id,
            g::proposer,
            g::targets,
            g::call_values,
            g::calldatas,
            g::description,
            g::vote_start,
            g::vote_end,
            g::quorum_vote_deadline,
            g::block_number,
            g::queued_at_block,
            g::eta,
            g::executed_at_block,
            g::canceled_at_block,
        ))
        .first(&mut conn)
        .await
        .unwrap();
    drop(conn);

    assert_eq!(row.0, BigDecimal::from(42));
    assert_eq!(row.1, vec![0x01; 20]);
    assert_eq!(row.2, vec![vec![0x02; 20], vec![0x03; 20]]);
    assert_eq!(row.3, vec![BigDecimal::from(0), BigDecimal::from(7)]);
    assert_eq!(row.4, vec![vec![0xab, 0xcd], vec![]]);
    assert_eq!(row.5, "# Burn fees\n\nBody");
    assert_eq!((row.6, row.7), ((TS + 1) as i64, (TS + 301) as i64));
    assert_eq!(
        row.8,
        Some((TS + 241) as i64),
        "quorum vote deadline applied after the insert in the same window"
    );
    assert_eq!(row.9, 100);
    assert_eq!((row.10, row.11), (Some(110), Some((TS + 400) as i64)));
    assert_eq!((row.12, row.13), (Some(120), None));

    assert_eq!(
        tallies(&pool, 42).await,
        vec![
            Tally {
                support: 0,
                weight: BigDecimal::from(50),
                votes: 1
            },
            Tally {
                support: 1,
                weight: BigDecimal::from(500),
                votes: 2
            },
            Tally {
                support: 2,
                weight: BigDecimal::from(5),
                votes: 1
            },
        ]
    );

    use database::schema::gov_votes as v;
    let mut conn = pool.get().await.unwrap();
    let params: Vec<Option<Vec<u8>>> = v::table
        .order(v::block_number.asc())
        .select(v::params)
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(params, vec![None, None, None, Some(vec![0x01, 0x02])]);
}

/// A rewound cursor replays the window over rows already written. Nothing
/// doubles, and a vote replayed does not inflate a tally.
#[tokio::test]
async fn replaying_the_window_converges() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN).await;
    seed_lifecycle(&pool).await;
    let ctx = ctx(pool.clone(), Some(governor()));
    let _ = tick_chain(&ctx, CHAIN, 500).await.unwrap();

    {
        use database::schema::consumer_cursors as c;
        let mut conn = pool.get().await.unwrap();
        diesel::update(c::table)
            .filter(c::name.eq(database::reorg::Owner::Protocol.cursor_name()))
            .set(c::last_event_id.eq(0))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let _ = tick_chain(&ctx, CHAIN, 500).await.unwrap();

    assert_eq!(count(&pool, "gov_proposals").await, 1);
    assert_eq!(count(&pool, "gov_votes").await, 4);
    assert_eq!(tallies(&pool, 42).await[1].weight, BigDecimal::from(500));
}

/// OpenZeppelin's signatures are generic, so the same topic0 from another
/// contract is not a proposal on this deployment. Without a governor configured
/// for the chain, nothing is accepted at all. Either way the cursor moves past.
#[tokio::test]
async fn events_from_another_emitter_or_without_a_governor_are_skipped() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN).await;
    let impostor = build_log_from(
        Address::repeat_byte(0x66),
        created(7).encode_log_data(),
        100,
        TS,
        0x20,
        0,
    );
    file(&pool, &impostor, EventKind::ProposalCreated).await;
    let genuine = gov_log(&created(8), 101, 0x21, 0);
    file(&pool, &genuine, EventKind::ProposalCreated).await;

    let _ = tick_chain(&ctx(pool.clone(), None), CHAIN, 500)
        .await
        .unwrap();
    assert_eq!(
        count(&pool, "gov_proposals").await,
        0,
        "no governor, no governance"
    );

    {
        use database::schema::consumer_cursors as c;
        let mut conn = pool.get().await.unwrap();
        let last: i64 = c::table
            .filter(c::name.eq(database::reorg::Owner::Protocol.cursor_name()))
            .select(c::last_event_id)
            .first(&mut conn)
            .await
            .unwrap();
        assert_eq!(last, 2, "skipped rows still advance the cursor");
        diesel::update(c::table)
            .set(c::last_event_id.eq(0))
            .execute(&mut conn)
            .await
            .unwrap();
    }

    let _ = tick_chain(&ctx(pool.clone(), Some(governor())), CHAIN, 500)
        .await
        .unwrap();
    use database::schema::gov_proposals as g;
    let mut conn = pool.get().await.unwrap();
    let ids: Vec<BigDecimal> = g::table
        .select(g::proposal_id)
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(ids, vec![BigDecimal::from_str("8").unwrap()]);
}

/// A governor deployed below the ingester's start block has votes and marks on
/// proposals never indexed. They must not fail the tick: the votes are kept and
/// the marks, having no row to land on, are dropped.
#[tokio::test]
async fn votes_and_marks_for_an_unseen_proposal_do_not_fail_the_tick() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN).await;
    file(
        &pool,
        &gov_log(&vote(0xb1, 99, 1, 10), 100, 0x30, 0),
        EventKind::VoteCast,
    )
    .await;
    let canceled = ProposalCanceled {
        proposalId: U256::from(99u64),
    };
    file(
        &pool,
        &gov_log(&canceled, 101, 0x31, 0),
        EventKind::ProposalCanceled,
    )
    .await;
    // And a live proposal canceled in the same window, which must be marked.
    file(
        &pool,
        &gov_log(&created(5), 102, 0x32, 0),
        EventKind::ProposalCreated,
    )
    .await;
    let canceled = ProposalCanceled {
        proposalId: U256::from(5u64),
    };
    file(
        &pool,
        &gov_log(&canceled, 103, 0x33, 0),
        EventKind::ProposalCanceled,
    )
    .await;

    let _ = tick_chain(&ctx(pool.clone(), Some(governor())), CHAIN, 500)
        .await
        .expect("an orphan vote is not an error");

    assert_eq!(
        count(&pool, "gov_votes").await,
        1,
        "the orphan vote is kept"
    );
    use database::schema::gov_proposals as g;
    let mut conn = pool.get().await.unwrap();
    let rows: Vec<(BigDecimal, Option<i64>)> = g::table
        .select((g::proposal_id, g::canceled_at_block))
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(rows, vec![(BigDecimal::from(5), Some(103))]);
}
