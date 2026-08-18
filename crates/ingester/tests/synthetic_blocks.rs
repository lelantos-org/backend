//! Synthetic-block stream tests for the ingester.
//!
//! Drives `worker::run_with_rpc` against a `MockRpc` script and a real
//! Postgres testcontainer. Asserts:
//! - rows ordered by `(chain_id, block_number, log_index)`, no gaps
//! - replay is idempotent (UNIQUE constraint absorbs duplicates)
//! - parent-hash mismatch triggers reorg rewind
//! - two chains advance independently with no row collisions
//!
//! Single shared Postgres container per test binary; per-test TRUNCATE.

use alloy::primitives::{Address, B256, Bytes, LogData};
use alloy::rpc::types::eth::Log;
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use chain_types::abi::NotePayload;
use database::advisory::{ChainLock, NS_INGESTER, chain_key};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use ingester::adapters::rpc::{BlockMeta, ChainRpc, DynRpc};
use ingester::app::config::ChainConfig;
use ingester::app::state::WorkerDeps;
use ingester::domain::error::IngesterError;
use ingester::domain::models::RawEvent;
use ingester::domain::models::{BlockCursor, TickOutcome, parse_address};
use ingester::repositories::{
    AtomicWriteRepo, ChainStateRepo, PostgresAtomicWriteRepo, PostgresChainStateRepo,
    PostgresRawEventRepo,
};
use ingester::services::backfill::BackfillService;
use ingester::services::ingest::IngestService;
use ingester::services::live::{LiveService, LiveServiceImpl};
use ingester::services::reorg::ReorgService;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

const POOL_ADDR: &str = "0x0000000000000000000000000000000000000abc";

// ---------- shared Postgres container ----------

struct ContainerHandle {
    _container: ContainerAsync<Postgres>,
    url: String,
}

async fn shared_container() -> &'static ContainerHandle {
    static CELL: OnceLock<tokio::sync::OnceCell<ContainerHandle>> = OnceLock::new();
    let cell = CELL.get_or_init(tokio::sync::OnceCell::new);
    cell.get_or_init(|| async {
        let container = Postgres::default().start().await.expect("start postgres");
        let host = container.get_host().await.unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@{}:{}/postgres", host, port);
        let migrate_url = url.clone();
        tokio::task::spawn_blocking(move || database::migrate::run(&migrate_url))
            .await
            .unwrap()
            .expect("migrate");
        ContainerHandle {
            _container: container,
            url,
        }
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
        .expect("pool");
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query(
        "TRUNCATE raw_events, chain_state, chain_reorgs, consumer_cursors, notes, \
         subscriptions, matches, assets \
         RESTART IDENTITY CASCADE",
    )
    .execute(&mut conn)
    .await
    .expect("truncate");
    drop(conn);
    (pool, guard)
}

// ---------- mock RPC ----------

#[derive(Clone)]
struct MockRpc {
    state: Arc<Mutex<MockState>>,
}

struct MockState {
    /// (block_number, block_hash, ts, logs_in_block)
    blocks: Vec<(u64, B256, u64, Vec<Log>)>,
    /// current visible tip; tests advance this to drip blocks in.
    tip: u64,
}

impl MockRpc {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState {
                blocks: Vec::new(),
                tip: 0,
            })),
        }
    }

    fn append(&self, block_n: u64, block_hash: B256, ts: u64, logs: Vec<Log>) {
        let mut s = self.state.lock().unwrap();
        s.blocks.push((block_n, block_hash, ts, logs));
        if block_n > s.tip {
            s.tip = block_n;
        }
    }

    /// Drop blocks ≥ `from`, then accept new ones (simulates a fork).
    fn rewind_to(&self, from: u64) {
        let mut s = self.state.lock().unwrap();
        s.blocks.retain(|(n, _, _, _)| *n < from);
        s.tip = s.blocks.iter().map(|(n, _, _, _)| *n).max().unwrap_or(0);
    }
}

#[async_trait]
impl ChainRpc for MockRpc {
    async fn tip(&self) -> Result<u64, IngesterError> {
        Ok(self.state.lock().unwrap().tip)
    }
    async fn fetch_logs(
        &self,
        _addr: Address,
        from: u64,
        to: u64,
    ) -> Result<Vec<Log>, IngesterError> {
        let s = self.state.lock().unwrap();
        let mut out = Vec::new();
        for (n, _, _, logs) in &s.blocks {
            if *n >= from && *n <= to {
                out.extend(logs.iter().cloned());
            }
        }
        Ok(out)
    }
    async fn block_hash_at(&self, n: u64) -> Result<Option<B256>, IngesterError> {
        let s = self.state.lock().unwrap();
        Ok(s.blocks
            .iter()
            .find(|(b, _, _, _)| *b == n)
            .map(|(_, h, _, _)| *h))
    }
    async fn fetch_block_meta(
        &self,
        blocks: &[u64],
    ) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
        let s = self.state.lock().unwrap();
        let mut out = HashMap::new();
        for &b in blocks {
            if let Some((_, _, ts, _)) = s.blocks.iter().find(|(n, _, _, _)| *n == b) {
                // Synthetic chain behaves like Ethereum: block.number is the
                // block's own height.
                out.insert(
                    b,
                    BlockMeta {
                        timestamp: *ts,
                        evm_block_number: b,
                    },
                );
            }
        }
        Ok(out)
    }
}

// ---------- log construction ----------

fn note_created_log(block_n: u64, block_hash: B256, tx_hash: B256, log_index: u64) -> Log {
    let cm = B256::repeat_byte(((block_n & 0xff) as u8).max(1));
    let ev = NotePayload {
        cm,
        clueRx: alloy::primitives::U256::from(0u64),
        clueRy: alloy::primitives::U256::from(0u64),
        ephPubX: alloy::primitives::U256::from(0u64),
        ephPubY: alloy::primitives::U256::from(0u64),
        ciphertext: Bytes::from(vec![0x00, 0x00, 1u8, 2, 3, 4, 5, 6, 7, 8]),
        cvDepX: alloy::primitives::U256::from(0u64),
        cvDepY: alloy::primitives::U256::from(0u64),
    };
    let log_data: LogData = ev.encode_log_data();
    let inner = alloy::primitives::Log {
        address: POOL_ADDR.parse::<Address>().unwrap(),
        data: log_data,
    };
    Log {
        inner,
        block_hash: Some(block_hash),
        block_number: Some(block_n),
        block_timestamp: Some(1_700_000_000 + block_n),
        transaction_hash: Some(tx_hash),
        transaction_index: Some(0),
        log_index: Some(log_index),
        removed: false,
    }
}

fn populate_blocks(rpc: &MockRpc, range: std::ops::RangeInclusive<u64>, hash_byte: u8) {
    for block_n in range {
        let block_hash = B256::repeat_byte(hash_byte ^ ((block_n & 0xff) as u8));
        let tx_hash = B256::repeat_byte(0x10 ^ ((block_n & 0xff) as u8));
        let log = note_created_log(block_n, block_hash, tx_hash, 0);
        rpc.append(block_n, block_hash, 1_700_000_000 + block_n, vec![log]);
    }
}

fn cfg(chain_id: i64, start_block: i64) -> ChainConfig {
    ChainConfig {
        chain_id,
        rpc_url: "mock".into(),
        pool_address: POOL_ADDR.into(),
        start_block,
        reorg_depth: 32,
        block_poll_ms: 10,
        backfill_threshold: 1_000_000, // disable backfill path
        backfill_concurrency: 1,
        chunk_blocks: 100,
        meta_concurrency: 4,
        rpc_timeout_ms: 5_000,
        rpc_connect_timeout_ms: 2_000,
    }
}

async fn live_ctx(
    pool: &database::DbPool,
    rpc: &Arc<MockRpc>,
    cfg: ChainConfig,
) -> LiveServiceImpl {
    let pool_addr = parse_address(&cfg.pool_address).unwrap();
    let writes = Arc::new(PostgresAtomicWriteRepo::new(pool.clone()));
    let raw_events = Arc::new(PostgresRawEventRepo::new(pool.clone()));
    let chain_state = Arc::new(PostgresChainStateRepo::new(pool.clone()));
    let ingest = Arc::new(IngestService::new(
        writes.clone(),
        raw_events.clone(),
        chain_state.clone(),
    ));
    let reorg = Arc::new(ReorgService::new(writes, raw_events, chain_state.clone()));
    LiveServiceImpl {
        cfg,
        pool_addr,
        rpc: rpc.clone() as DynRpc,
        chain_state,
        ingest,
        reorg,
    }
}

async fn drain_ticks(ctx: &LiveServiceImpl, max: usize) -> Vec<TickOutcome> {
    let mut out = Vec::new();
    for _ in 0..max {
        let r = ctx.tick().await.unwrap();
        let done = matches!(r, TickOutcome::Idle);
        out.push(r);
        if done {
            break;
        }
    }
    out
}

// ---------- assertions ----------

#[derive(Debug, Clone, Queryable)]
#[allow(dead_code)]
struct EventRow {
    id: i64,
    chain_id: i64,
    block_number: i64,
    log_index: i32,
}

async fn fetch_all_events(pool: &database::DbPool, chain_id_filter: Option<i64>) -> Vec<EventRow> {
    use database::schema::raw_events;
    let mut conn = pool.get().await.unwrap();
    let mut q = raw_events::table.into_boxed();
    if let Some(c) = chain_id_filter {
        q = q.filter(raw_events::chain_id.eq(c));
    }
    q.order(raw_events::id.asc())
        .select((
            raw_events::id,
            raw_events::chain_id,
            raw_events::block_number,
            raw_events::log_index,
        ))
        .load::<EventRow>(&mut conn)
        .await
        .unwrap()
}

// ---------- tests ----------

#[tokio::test]
async fn single_chain_orders_rows() {
    let (pool, _serial) = fresh_pool().await;
    let rpc = Arc::new(MockRpc::new());
    populate_blocks(&rpc, 100..=109, 0xa0);
    let ctx = live_ctx(&pool, &rpc, cfg(1, 100)).await;

    let _ = drain_ticks(&ctx, 5).await;

    let rows = fetch_all_events(&pool, None).await;
    assert_eq!(rows.len(), 10, "10 blocks × 1 log each");
    let blocks: Vec<i64> = rows.iter().map(|r| r.block_number).collect();
    assert_eq!(blocks, (100..=109).collect::<Vec<_>>(), "ordered, no gaps");
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "ids monotone");
}

#[tokio::test]
async fn idempotent_replay() {
    let (pool, _serial) = fresh_pool().await;
    let rpc = Arc::new(MockRpc::new());
    populate_blocks(&rpc, 200..=204, 0xb0);
    let ctx = live_ctx(&pool, &rpc, cfg(1, 200)).await;

    let _ = drain_ticks(&ctx, 5).await;
    let first = fetch_all_events(&pool, None).await;
    assert_eq!(first.len(), 5);

    // Manually reset cursor to replay; rows should not duplicate.
    {
        use database::schema::chain_state;
        let mut conn = pool.get().await.unwrap();
        diesel::update(chain_state::table)
            .set(chain_state::last_scanned_block.eq(199))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let _ = drain_ticks(&ctx, 5).await;
    let second = fetch_all_events(&pool, None).await;
    assert_eq!(second.len(), first.len(), "no dupes; UNIQUE absorbs");
}

#[tokio::test]
async fn reorg_rewinds_correctly() {
    let (pool, _serial) = fresh_pool().await;
    let rpc = Arc::new(MockRpc::new());

    populate_blocks(&rpc, 300..=305, 0xa0);
    let ctx = live_ctx(&pool, &rpc, cfg(1, 300)).await;
    let _ = drain_ticks(&ctx, 5).await;
    assert_eq!(fetch_all_events(&pool, None).await.len(), 6);

    // Fork at block 304: rewind RPC and feed a different hash.
    rpc.rewind_to(304);
    populate_blocks(&rpc, 304..=308, 0xff); // different hash_byte → divergent block_hash

    // No cursor reset. That is the whole point: detection has to notice the
    // fork from the stored anchor alone. The previous implementation compared
    // incoming logs against stored ones, which can only ever match if
    // something has already rewound the cursor by hand — so this test used to
    // pass while production could never detect a reorg at all.
    let first = ctx.tick().await.unwrap();
    assert!(
        matches!(first, TickOutcome::Reorg { .. }),
        "got {:?}",
        first
    );

    // Subsequent ticks ingest the new branch.
    let _ = drain_ticks(&ctx, 5).await;
    let rows = fetch_all_events(&pool, None).await;
    let blocks: Vec<i64> = rows.iter().map(|r| r.block_number).collect();
    assert_eq!(blocks, (300..=308).collect::<Vec<_>>(), "canonical chain");
}

#[tokio::test]
async fn multichain_independent_cursors() {
    let (pool, _serial) = fresh_pool().await;
    let rpc1 = Arc::new(MockRpc::new());
    let rpc2 = Arc::new(MockRpc::new());
    populate_blocks(&rpc1, 100..=104, 0x10);
    populate_blocks(&rpc2, 500..=509, 0x20);

    let ctx1 = live_ctx(&pool, &rpc1, cfg(1, 100)).await;
    let ctx2 = live_ctx(&pool, &rpc2, cfg(8453, 500)).await;

    let h1 = tokio::spawn(async move {
        for _ in 0..6 {
            let r = ctx1.tick().await.unwrap();
            if matches!(r, TickOutcome::Idle) {
                break;
            }
        }
    });
    let h2 = tokio::spawn(async move {
        for _ in 0..12 {
            let r = ctx2.tick().await.unwrap();
            if matches!(r, TickOutcome::Idle) {
                break;
            }
        }
    });
    let _ = tokio::join!(h1, h2);

    let c1 = fetch_all_events(&pool, Some(1)).await;
    let c2 = fetch_all_events(&pool, Some(8453)).await;
    assert_eq!(c1.len(), 5, "chain 1");
    assert_eq!(c2.len(), 10, "chain 8453");
    assert_eq!(
        c1.iter().map(|r| r.block_number).collect::<Vec<_>>(),
        (100..=104).collect::<Vec<_>>()
    );
    assert_eq!(
        c2.iter().map(|r| r.block_number).collect::<Vec<_>>(),
        (500..=509).collect::<Vec<_>>()
    );
}

// ---------- replica failover ----------

async fn db_url() -> &'static str {
    &shared_container().await.url
}

fn worker_deps(
    pool: &database::DbPool,
    rpc: &Arc<MockRpc>,
    cfg: ChainConfig,
    url: &str,
) -> WorkerDeps {
    let writes = Arc::new(PostgresAtomicWriteRepo::new(pool.clone()));
    let raw_events = Arc::new(PostgresRawEventRepo::new(pool.clone()));
    let chain_state = Arc::new(PostgresChainStateRepo::new(pool.clone()));
    let ingest = Arc::new(IngestService::new(
        writes.clone(),
        raw_events.clone(),
        chain_state.clone(),
    ));
    let reorg = Arc::new(ReorgService::new(writes, raw_events, chain_state.clone()));
    let backfill = Arc::new(BackfillService::new(rpc.clone() as DynRpc, ingest.clone()));
    WorkerDeps {
        cfg,
        rpc: rpc.clone() as DynRpc,
        chain_state,
        ingest,
        reorg,
        backfill,
        database_url: url.to_string(),
    }
}

async fn count_raw_events(pool: &database::DbPool) -> i64 {
    use database::schema::raw_events;
    let mut conn = pool.get().await.unwrap();
    raw_events::table
        .count()
        .get_result(&mut conn)
        .await
        .unwrap()
}

/// A second ingester replica must stand by rather than ingest beside the
/// leader — and must take over once the leader's lock goes away. Returning
/// early instead of retrying would leave the standby permanently inert, so
/// there would be no failover at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standby_ingester_waits_for_the_lock_then_takes_over() {
    let (pool, _serial) = fresh_pool().await;
    let url = db_url().await;
    let rpc = Arc::new(MockRpc::new());
    populate_blocks(&rpc, 100..=109, 0xa0);

    let leader = ChainLock::try_acquire(url, chain_key(NS_INGESTER, 1))
        .await
        .unwrap()
        .expect("leader lock");

    let deps = worker_deps(&pool, &rpc, cfg(1, 100), url);
    // Hold the trigger: dropping it closes the watch channel, which every
    // worker reads as "shutdown now".
    let (_trigger, shutdown) = shared::shutdown::channel();
    let worker = tokio::spawn(async move { ingester::handlers::worker::run(deps, shutdown).await });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        count_raw_events(&pool).await,
        0,
        "standby must not ingest while the leader holds the lock"
    );
    assert!(
        !worker.is_finished(),
        "standby must keep retrying, not exit"
    );

    drop(leader);

    let mut ingested = 0;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        ingested = count_raw_events(&pool).await;
        if ingested > 0 {
            break;
        }
    }
    assert!(ingested > 0, "standby must take over once the lock frees");
    worker.abort();
}

/// A fork whose replacement block emits no logs at all.
///
/// This is unreachable for any scheme that compares incoming logs against
/// stored ones: the diverged block contributes no incoming row, so there is
/// nothing to compare it to. The anchor walk finds it anyway, and must rewind
/// to the *lowest* diverged block — picking any other diverged block (the old
/// code iterated a HashMap and returned whichever came first) would strand the
/// rows below it forever.
#[tokio::test]
async fn detects_a_fork_whose_replacement_block_has_no_logs() {
    let (pool, _serial) = fresh_pool().await;
    let rpc = Arc::new(MockRpc::new());

    populate_blocks(&rpc, 400..=405, 0xa0);
    let ctx = live_ctx(&pool, &rpc, cfg(1, 400)).await;
    let _ = drain_ticks(&ctx, 8).await;
    assert_eq!(fetch_all_events(&pool, None).await.len(), 6);

    // New branch from 403. 403 itself carries no logs; 404 and 405 do.
    rpc.rewind_to(403);
    let h403 = B256::repeat_byte(0xf0);
    rpc.append(403, h403, 1_700_000_403, vec![]);
    populate_blocks(&rpc, 404..=405, 0xff);

    let first = ctx.tick().await.unwrap();
    match first {
        TickOutcome::Reorg { rewind_to } => assert_eq!(
            rewind_to, 403,
            "must rewind to the lowest diverged block, not an arbitrary one"
        ),
        other => panic!("expected a reorg, got {other:?}"),
    }

    let _ = drain_ticks(&ctx, 8).await;
    let blocks: Vec<i64> = fetch_all_events(&pool, None)
        .await
        .iter()
        .map(|r| r.block_number)
        .collect();
    assert_eq!(
        blocks,
        vec![400, 401, 402, 404, 405],
        "403's orphaned row is gone and the new branch is ingested"
    );
}

/// A chain whose scanned range contains no matching logs must still record
/// progress.
///
/// `advance_scanned` used to be a bare `UPDATE`, which matches zero rows until
/// something has committed an event and created the `chain_state` row. The
/// cursor therefore never persisted, and every poll rescanned the same
/// widening range from `start_block` forever.
#[tokio::test]
async fn advances_the_cursor_on_a_range_with_no_logs() {
    let (pool, _serial) = fresh_pool().await;
    let rpc = Arc::new(MockRpc::new());
    for block_n in 500..=510u64 {
        rpc.append(
            block_n,
            B256::repeat_byte((block_n & 0xff) as u8),
            1_700_000_000 + block_n,
            vec![],
        );
    }
    let ctx = live_ctx(&pool, &rpc, cfg(1, 500)).await;

    let first = ctx.tick().await.unwrap();
    assert!(
        matches!(first, TickOutcome::Empty { to: 510 }),
        "got {first:?}"
    );

    let chain_state = PostgresChainStateRepo::new(pool.clone());
    let cursor = chain_state
        .fetch(1)
        .await
        .unwrap()
        .expect("an empty range must still persist a cursor");
    assert_eq!(cursor.last_scanned_block, 510);
    // The anchor must stay empty: nothing was verified, so there is no block
    // for a later reorg check to walk back from.
    assert_eq!(cursor.last_block_hash, Vec::<u8>::new());

    let second = ctx.tick().await.unwrap();
    assert!(
        matches!(second, TickOutcome::Idle),
        "the range must not be rescanned, got {second:?}"
    );
}

/// Postgres caps a statement at 65535 bind parameters and each row binds 10,
/// so a single-statement insert tops out at 6553 rows — well under what one
/// backfill chunk of the default 50k blocks can produce.
#[tokio::test]
async fn inserts_a_batch_larger_than_the_bind_parameter_limit() {
    let (pool, _serial) = fresh_pool().await;
    let writes = PostgresAtomicWriteRepo::new(pool.clone());

    const N: usize = 7_000;
    let rows: Vec<RawEvent> = (0..N)
        .map(|i| RawEvent {
            chain_id: 1,
            block_number: 1_000 + (i as i64 / 10),
            evm_block_number: 1_000 + (i as i64 / 10),
            block_hash: vec![0xab; 32],
            block_ts: 1_700_000_000,
            tx_hash: vec![0xcd; 32],
            log_index: (i % 10) as i32,
            event_kind: 0,
            topics: vec![vec![0u8; 32]],
            data: vec![1, 2, 3],
        })
        .collect();

    let inserted = writes
        .commit_batch(
            &rows,
            &BlockCursor {
                chain_id: 1,
                last_block: 1_000 + (N as i64 / 10),
                last_block_hash: vec![0xab; 32],
                last_scanned_block: 1_000 + (N as i64 / 10),
            },
        )
        .await
        .expect("a batch over the bind-parameter limit must still commit");
    assert_eq!(inserted, N);
    assert_eq!(count_raw_events(&pool).await, N as i64);

    // Replaying the same batch must report zero inserted, not N. Reporting the
    // decoded count made every replay look like fresh ingest.
    let again = writes
        .commit_batch(
            &rows,
            &BlockCursor {
                chain_id: 1,
                last_block: 1_000 + (N as i64 / 10),
                last_block_hash: vec![0xab; 32],
                last_scanned_block: 1_000 + (N as i64 / 10),
            },
        )
        .await
        .unwrap();
    assert_eq!(again, 0, "duplicates absorbed by the unique index");
    assert_eq!(count_raw_events(&pool).await, N as i64);
}

/// A reorg must leave a durable, atomic record.
///
/// `pg_notify` is fire-and-forget, so a consumer that is down when the fork
/// happens never hears about it. Consumers re-read the replacement rows on
/// their own — the ids are higher — but state they already derived from the
/// deleted rows sits below their cursor where nothing revisits it. The log row
/// is what lets them retract it, so it has to be written in the same
/// transaction as the delete.
#[tokio::test]
async fn a_rewind_records_a_durable_reorg_marker() {
    let (pool, _serial) = fresh_pool().await;
    let rpc = Arc::new(MockRpc::new());

    populate_blocks(&rpc, 600..=605, 0xa0);
    let ctx = live_ctx(&pool, &rpc, cfg(1, 600)).await;
    let _ = drain_ticks(&ctx, 8).await;

    rpc.rewind_to(604);
    populate_blocks(&rpc, 604..=606, 0xff);
    let outcome = ctx.tick().await.unwrap();
    assert!(
        matches!(outcome, TickOutcome::Reorg { rewind_to: 604 }),
        "got {outcome:?}"
    );

    let pending = database::reorg::pending(&pool, 1, 0).await.unwrap();
    assert_eq!(pending.len(), 1, "one rewind, one marker");
    assert_eq!(pending[0].rewind_to, 604);
    assert_eq!(pending[0].chain_id, 1);

    // A consumer that has already read past the fork retracts and replays.
    // Nothing derived exists in this test, so the assertion is on the
    // bookkeeping: the reorg is consumed exactly once.
    {
        use database::schema::consumer_cursors;
        let mut conn = pool.get().await.unwrap();
        diesel::insert_into(consumer_cursors::table)
            .values((
                consumer_cursors::name.eq("fmd"),
                consumer_cursors::chain_id.eq(1i64),
                consumer_cursors::last_event_id.eq(999i64),
                consumer_cursors::last_block_number.eq(605i64),
            ))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let applied = database::reorg::apply_pending(&pool, "fmd", 1)
        .await
        .unwrap();
    assert_eq!(applied, 1);
    let again = database::reorg::apply_pending(&pool, "fmd", 1)
        .await
        .unwrap();
    assert_eq!(
        again, 0,
        "an applied reorg must not be re-applied every tick"
    );

    let (after, _) = {
        use database::CursorRepo;
        database::PostgresCursorRepo::new(pool.clone())
            .fetch("fmd", 1)
            .await
            .unwrap()
    };
    assert_eq!(
        after, 0,
        "cursor rewound so the consumer replays the branch"
    );
}
