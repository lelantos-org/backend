//! Fixture-replay tests for explorer-indexer against the current ABI:
//! `AssetRegistered` + `RootAdvanced`.

use alloy::primitives::{Address, B256, LogData, U256};
use alloy::rpc::types::eth::Log;
use alloy::sol_types::SolEvent;
use bigdecimal::BigDecimal;
use chain_types::abi::{AssetRegistered, RootAdvanced};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use explorer_indexer::config::ExplorerIndexerConfig;
use explorer_indexer::services::consume::{ConsumeCtx, tick_chain};
use shared::entities::EventKind;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

const POOL_ADDR: &str = "0x0000000000000000000000000000000000000abc";
const CHAIN_A: i64 = 1;
const ASSET_ID: u64 = 7;

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
    diesel::sql_query(
        "TRUNCATE raw_events, chain_state, consumer_cursors, notes, \
         subscriptions, matches, assets, tree_advances \
         RESTART IDENTITY CASCADE",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);
    (pool, guard)
}

async fn insert_chain_state(pool: &database::DbPool, chain_id: i64) {
    use database::schema::chain_state;
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(chain_state::table)
        .values((
            chain_state::chain_id.eq(chain_id),
            chain_state::last_block.eq(0i64),
            chain_state::last_block_hash.eq::<Vec<u8>>(vec![0u8; 32]),
            chain_state::last_scanned_block.eq(0i64),
        ))
        .execute(&mut conn)
        .await
        .unwrap();
}

#[derive(Insertable)]
#[diesel(table_name = database::schema::raw_events)]
struct InsertableRawEvent {
    chain_id: i64,
    block_number: i64,
    block_hash: Vec<u8>,
    block_ts: i64,
    tx_hash: Vec<u8>,
    log_index: i32,
    event_kind: i16,
    topics: Vec<Vec<u8>>,
    data: Vec<u8>,
}

fn pool_addr() -> Address {
    POOL_ADDR.parse().unwrap()
}

fn build_log(log_data: LogData, block_n: u64, block_ts: u64, tx_byte: u8, log_idx: u64) -> Log {
    Log {
        inner: alloy::primitives::Log {
            address: pool_addr(),
            data: log_data,
        },
        block_hash: Some(B256::repeat_byte(0xaa)),
        block_number: Some(block_n),
        block_timestamp: Some(block_ts),
        transaction_hash: Some(B256::repeat_byte(tx_byte)),
        transaction_index: Some(0),
        log_index: Some(log_idx),
        removed: false,
    }
}

fn asset_registered_log(
    asset_id: u64,
    token_byte: u8,
    block_n: u64,
    block_ts: u64,
    tx_byte: u8,
    log_idx: u64,
) -> Log {
    let ev = AssetRegistered {
        assetId: asset_id,
        token: Address::repeat_byte(token_byte),
        scale: U256::from(1_000_000u64),
    };
    build_log(ev.encode_log_data(), block_n, block_ts, tx_byte, log_idx)
}

fn root_advanced_log(
    start_index: u64,
    inserted: u64,
    block_n: u64,
    block_ts: u64,
    tx_byte: u8,
    log_idx: u64,
) -> Log {
    let ev = RootAdvanced {
        startIndex: start_index,
        inserted,
        oldRoot: B256::repeat_byte(0xee),
        newRoot: B256::repeat_byte(0xff),
    };
    build_log(ev.encode_log_data(), block_n, block_ts, tx_byte, log_idx)
}

async fn insert_log(pool: &database::DbPool, chain_id: i64, log: &Log, kind: EventKind) {
    use database::schema::raw_events;
    let topics: Vec<Vec<u8>> = log.topics().iter().map(|t| t.0.to_vec()).collect();
    let row = InsertableRawEvent {
        chain_id,
        block_number: log.block_number.unwrap() as i64,
        block_hash: log.block_hash.unwrap().0.to_vec(),
        block_ts: log.block_timestamp.unwrap() as i64,
        tx_hash: log.transaction_hash.unwrap().0.to_vec(),
        log_index: log.log_index.unwrap() as i32,
        event_kind: kind.as_i16(),
        topics,
        data: log.data().data.to_vec(),
    };
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(raw_events::table)
        .values(&row)
        .execute(&mut conn)
        .await
        .unwrap();
}

/// No chains and no metadata RPC: the decimals sweep is a no-op here, so
/// these tests exercise event consumption alone.
fn empty_ctx(pool: database::DbPool) -> ConsumeCtx {
    ConsumeCtx {
        pool,
        cfg: Arc::new(ExplorerIndexerConfig {
            database_url: String::new(),
            chains: Vec::new(),
            tick_ms: 1000,
            batch: 500,
        }),
        token_meta: Arc::new(std::collections::HashMap::new()),
    }
}

#[tokio::test]
async fn asset_registered_persists_registry_fields() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    insert_log(
        &pool,
        CHAIN_A,
        &asset_registered_log(ASSET_ID, 0xde, 100, 1_700_000_000, 0x01, 0),
        EventKind::AssetRegistered,
    )
    .await;

    let ctx = empty_ctx(pool.clone());
    tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    use database::schema::assets;
    let mut conn = pool.get().await.unwrap();
    let row: (i64, Vec<u8>, BigDecimal) = assets::table
        .filter(assets::chain_id.eq(CHAIN_A))
        .select((assets::asset_id_u64, assets::token, assets::scale))
        .first(&mut conn)
        .await
        .unwrap();
    assert_eq!(row.0, ASSET_ID as i64);
    assert_eq!(row.1[0], 0xde);
    assert_eq!(row.2, BigDecimal::from_str("1000000").unwrap());
}

#[tokio::test]
async fn root_advanced_appends_tree_advances() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    insert_log(
        &pool,
        CHAIN_A,
        &root_advanced_log(0, 2, 100, 1_700_000_000, 0x10, 0),
        EventKind::RootAdvanced,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &root_advanced_log(2, 2, 101, 1_700_000_060, 0x11, 0),
        EventKind::RootAdvanced,
    )
    .await;

    let ctx = empty_ctx(pool.clone());
    tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    use database::schema::tree_advances;
    let mut conn = pool.get().await.unwrap();
    let rows: Vec<(i64, i64, i32)> = tree_advances::table
        .filter(tree_advances::chain_id.eq(CHAIN_A))
        .order(tree_advances::block_number.asc())
        .select((
            tree_advances::block_number,
            tree_advances::start_index,
            tree_advances::inserted,
        ))
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(rows, vec![(100, 0, 2), (101, 2, 2)]);
}

#[tokio::test]
async fn idempotent_replay_keeps_single_row_per_advance() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    insert_log(
        &pool,
        CHAIN_A,
        &asset_registered_log(ASSET_ID, 0xde, 100, 1_700_000_000, 0x01, 0),
        EventKind::AssetRegistered,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &root_advanced_log(0, 2, 101, 1_700_000_060, 0x10, 0),
        EventKind::RootAdvanced,
    )
    .await;

    let ctx = empty_ctx(pool.clone());
    tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    {
        use database::schema::consumer_cursors;
        let mut conn = pool.get().await.unwrap();
        diesel::update(consumer_cursors::table)
            .filter(consumer_cursors::name.eq("explorer"))
            .filter(consumer_cursors::chain_id.eq(CHAIN_A))
            .set(consumer_cursors::last_event_id.eq(0))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    use database::schema::{assets, tree_advances};
    let mut conn = pool.get().await.unwrap();
    let asset_count: i64 = assets::table.count().get_result(&mut conn).await.unwrap();
    let tree_count: i64 = tree_advances::table
        .count()
        .get_result(&mut conn)
        .await
        .unwrap();
    assert_eq!(asset_count, 1, "AssetRegistered upsert");
    assert_eq!(
        tree_count, 1,
        "tree_advances PK (chain, block, log_index) prevents duplicates"
    );
}
