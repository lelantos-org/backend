//! DB-backed tests for the pending-deposit query the flush worker runs every
//! tick.
//!
//! Both behaviours here are load-bearing for liveness: quarantined deposits
//! have to be excluded in SQL (they are the oldest rows, so post-filtering
//! would let them eat the whole `LIMIT` window), and one unreadable row must
//! not fail the query — the worker re-runs it forever, so an error there stops
//! the chain flushing for good.

use bigdecimal::BigDecimal;
use bigdecimal::FromPrimitive;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use relayer::services::deposit_mempool::DepositMempool;
use std::sync::{Arc, OnceLock};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

const CHAIN: i64 = 1;

struct ContainerHandle {
    _container: ContainerAsync<Postgres>,
    url: String,
}

async fn shared_container() -> &'static ContainerHandle {
    static CELL: OnceLock<tokio::sync::OnceCell<ContainerHandle>> = OnceLock::new();
    let cell = CELL.get_or_init(tokio::sync::OnceCell::new);
    cell.get_or_init(|| async {
        let c = Postgres::default().start().await.unwrap();
        let host = c.get_host().await.unwrap();
        let port = c.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@{}:{}/postgres", host, port);
        let migrate_url = url.clone();
        tokio::task::spawn_blocking(move || database::migrate::run(&migrate_url))
            .await
            .unwrap()
            .unwrap();
        ContainerHandle { _container: c, url }
    })
    .await
}

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    static SERIAL: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    let mu = SERIAL
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let guard = mu.lock_owned().await;
    let h = shared_container().await;
    let pool = database::build_pool(&h.url, database::PoolCfg::indexer())
        .await
        .unwrap();
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query("TRUNCATE deposit_escrowed_events RESTART IDENTITY CASCADE")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    (pool, guard)
}

#[derive(Insertable)]
#[diesel(table_name = database::schema::deposit_escrowed_events)]
struct NewDeposit {
    chain_id: i64,
    block_number: i64,
    log_index: i32,
    deposit_id: BigDecimal,
    payer: Vec<u8>,
    recipient: Vec<u8>,
    public_asset_id: i64,
    public_in: BigDecimal,
    fee_bps_at_submit: i32,
    cm: Vec<u8>,
    cv_dep_x: BigDecimal,
    cv_dep_y: BigDecimal,
    rcv: BigDecimal,
    aux: serde_json::Value,
    submitted_at_block: i64,
    tx_hash: Vec<u8>,
    block_ts: i64,
}

fn deposit(id: u64) -> NewDeposit {
    let n = |v: u64| BigDecimal::from_u64(v).unwrap();
    NewDeposit {
        chain_id: CHAIN,
        block_number: id as i64,
        log_index: 0,
        deposit_id: n(id),
        payer: vec![0x11; 20],
        recipient: vec![0x22; 20],
        public_asset_id: 7,
        public_in: n(1_000),
        fee_bps_at_submit: 25,
        cm: vec![0x33; 32],
        cv_dep_x: n(3),
        cv_dep_y: n(4),
        rcv: n(5),
        aux: serde_json::json!({}),
        submitted_at_block: id as i64,
        tx_hash: vec![0x44; 32],
        block_ts: 1_700_000_000,
    }
}

async fn insert(pool: &database::DbPool, rows: Vec<NewDeposit>) {
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(database::schema::deposit_escrowed_events::table)
        .values(rows)
        .execute(&mut conn)
        .await
        .unwrap();
}

#[tokio::test]
async fn excluded_deposits_do_not_consume_the_limit_window() {
    let (pool, _guard) = fresh_pool().await;
    insert(&pool, (1..=4).map(deposit).collect()).await;
    let mempool = DepositMempool::new(pool, CHAIN);

    let ids = |v: Vec<_>| -> Vec<u64> {
        v.into_iter()
            .map(|d: relayer::services::deposit_mempool::PendingDeposit| d.id)
            .collect()
    };

    assert_eq!(ids(mempool.pop_pending(2, &[]).await.unwrap()), vec![1, 2]);
    // The two oldest are quarantined. Excluding them after the query would
    // return an empty batch and starve deposits 3 and 4 forever.
    assert_eq!(
        ids(mempool.pop_pending(2, &[1, 2]).await.unwrap()),
        vec![3, 4]
    );
}

#[tokio::test]
async fn one_unreadable_row_does_not_fail_the_query() {
    let (pool, _guard) = fresh_pool().await;
    let mut rows: Vec<NewDeposit> = (1..=3).map(deposit).collect();
    // `submitted_at_block` past `uint32`: the contract hashed a `uint32`, so
    // this row can never produce a matching digest and cannot be parsed.
    rows[1].submitted_at_block = i64::from(u32::MAX) + 1;
    insert(&pool, rows).await;
    let mempool = DepositMempool::new(pool, CHAIN);

    let got = mempool.pop_pending(8, &[]).await.unwrap();
    assert_eq!(
        got.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![1, 3],
        "the readable rows must still flush"
    );
}

#[tokio::test]
async fn flushed_and_canceled_deposits_are_not_pending() {
    use database::schema::deposit_escrowed_events as t;
    let (pool, _guard) = fresh_pool().await;
    insert(&pool, (1..=3).map(deposit).collect()).await;
    let mut conn = pool.get().await.unwrap();
    diesel::update(t::table.filter(t::block_number.eq(1)))
        .set(t::flushed_at_block.eq(Some(10i64)))
        .execute(&mut conn)
        .await
        .unwrap();
    diesel::update(t::table.filter(t::block_number.eq(2)))
        .set(t::canceled_at_block.eq(Some(11i64)))
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let mempool = DepositMempool::new(pool, CHAIN);
    let got = mempool.pop_pending(8, &[]).await.unwrap();
    assert_eq!(got.iter().map(|d| d.id).collect::<Vec<_>>(), vec![3]);
}
