//! Fixture-replay tests for fmd-indexer against the current ABI:
//! `RootAdvanced` + `NotesCreated` (no `NullifierUsed`).

use alloy::primitives::{Address, B256, Bytes, LogData, U256};
use alloy::rpc::types::eth::Log;
use alloy::sol_types::SolEvent;
use ark_ed_on_bn254::Fq;
use ark_ff::{BigInteger, PrimeField};
use chain_types::abi::{NotePayload, NullifierConsumed, RootAdvanced};
use database::advisory::ChainLock;
use database::{CursorRepo, UpsertCursor};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use fmd_crypto::clue;
use fmd_indexer::adapters::locks::ChainLocks;
use fmd_indexer::repositories::cursor::PostgresCursorRepo;
use fmd_indexer::repositories::matches::PostgresMatchesRepo;
use fmd_indexer::repositories::notes::PostgresNotesRepo;
use fmd_indexer::repositories::raw_events::PostgresRawEventsRepo;
use fmd_indexer::repositories::spent_nullifiers::{
    NewSpentNullifier, PostgresSpentNullifiersRepo, SpentNullifiersRepo,
};
use fmd_indexer::repositories::subscriptions::{PostgresSubscriptionsRepo, SubscriptionsRepo};
use fmd_indexer::services::consume::{ConsumeService, ConsumeServiceImpl};
use fmd_indexer::services::filter::{FilterService, FilterServiceImpl};
use shared::entities::EventKind;
use std::sync::{Arc, OnceLock};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

const POOL_ADDR: &str = "0x0000000000000000000000000000000000000abc";
const CHAIN_A: i64 = 1;
const CHAIN_B: i64 = 8453;

const GAMMA3_DK: [&str; 3] = [
    "4199809491263568835236997",
    "642067096274462606251208960760534960",
    "8478110259546489282125397380691114815",
];
const GAMMA3_R_PACKED_HEX: &str =
    "2e328d3fde9e94a1a469bd7711e510cc003e90972309e2295915af03ad333e85";
const GAMMA3_BITS_LE: u16 = 0x0007;
const GAMMA3_GAMMA: i32 = 3;

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

async fn db_url() -> &'static str {
    &shared_container().await.url
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
        "TRUNCATE raw_events, chain_state, chain_reorgs, consumer_cursors, notes, \
         subscriptions, matches, assets, tree_advances, spent_nullifiers \
         RESTART IDENTITY CASCADE",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);
    (pool, guard)
}

fn build_consume(pool: &database::DbPool) -> ConsumeServiceImpl {
    ConsumeServiceImpl::new(
        pool.clone(),
        Arc::new(PostgresCursorRepo::new(pool.clone())),
        Arc::new(PostgresRawEventsRepo::new(pool.clone())),
        Arc::new(PostgresNotesRepo::new(pool.clone())),
        Arc::new(PostgresSpentNullifiersRepo::new(pool.clone())),
        ChainLocks::disabled(),
    )
}

fn build_filter(pool: &database::DbPool) -> FilterServiceImpl {
    FilterServiceImpl::new(
        Arc::new(PostgresCursorRepo::new(pool.clone())),
        Arc::new(PostgresNotesRepo::new(pool.clone())),
        Arc::new(PostgresSubscriptionsRepo::new(pool.clone())),
        Arc::new(PostgresMatchesRepo::new(pool.clone())),
    )
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

fn fq_to_u256(v: Fq) -> U256 {
    let bytes = v.into_bigint().to_bytes_be();
    let mut padded = [0u8; 32];
    let off = 32 - bytes.len();
    padded[off..].copy_from_slice(&bytes);
    U256::from_be_bytes(padded)
}

fn build_log(log_data: LogData, block_n: u64, tx_byte: u8, log_idx: u64) -> Log {
    Log {
        inner: alloy::primitives::Log {
            address: pool_addr(),
            data: log_data,
        },
        block_hash: Some(B256::repeat_byte(0xaa)),
        block_number: Some(block_n),
        block_timestamp: Some(1_700_000_000 + block_n),
        transaction_hash: Some(B256::repeat_byte(tx_byte)),
        transaction_index: Some(0),
        log_index: Some(log_idx),
        removed: false,
    }
}

fn root_advanced_log(
    start_index: u64,
    inserted: u64,
    block_n: u64,
    tx_byte: u8,
    log_idx: u64,
) -> Log {
    let ev = RootAdvanced {
        startIndex: start_index,
        inserted,
        oldRoot: B256::repeat_byte(0xee),
        newRoot: B256::repeat_byte(0xff),
    };
    build_log(ev.encode_log_data(), block_n, tx_byte, log_idx)
}

/// Emit one `NotePayload` log. The on-chain MASP emits one per output leaf,
/// each decoding to a single `DecodedEvent::NoteCreated`.
#[allow(clippy::too_many_arguments)]
fn note_payload_log(
    cm_byte: u8,
    clue_rx: U256,
    clue_ry: U256,
    clue_bits_u16: u16,
    body: Vec<u8>,
    block_n: u64,
    tx_byte: u8,
    log_idx: u64,
) -> Log {
    let mut ciphertext = Vec::with_capacity(2 + body.len());
    ciphertext.extend_from_slice(&clue_bits_u16.to_be_bytes());
    ciphertext.extend_from_slice(&body);
    note_payload_log_raw(
        cm_byte, clue_rx, clue_ry, ciphertext, block_n, tx_byte, log_idx,
    )
}

/// `note_payload_log` without the clueBits prefix, for ciphertexts that are
/// deliberately too short to carry one.
#[allow(clippy::too_many_arguments)]
fn note_payload_log_raw(
    cm_byte: u8,
    clue_rx: U256,
    clue_ry: U256,
    ciphertext: Vec<u8>,
    block_n: u64,
    tx_byte: u8,
    log_idx: u64,
) -> Log {
    let ev = NotePayload {
        cm: B256::repeat_byte(cm_byte),
        clueRx: clue_rx,
        clueRy: clue_ry,
        ephPubX: U256::from(0u64),
        ephPubY: U256::from(0u64),
        ciphertext: Bytes::from(ciphertext),
        cvDepX: U256::from(0u64),
        cvDepY: U256::from(0u64),
    };
    build_log(ev.encode_log_data(), block_n, tx_byte, log_idx)
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

async fn count_notes(pool: &database::DbPool, chain_id: i64) -> i64 {
    use database::schema::notes;
    let mut conn = pool.get().await.unwrap();
    notes::table
        .filter(notes::chain_id.eq(chain_id))
        .count()
        .get_result(&mut conn)
        .await
        .unwrap()
}

async fn count_matches(pool: &database::DbPool) -> i64 {
    use database::schema::matches;
    let mut conn = pool.get().await.unwrap();
    matches::table.count().get_result(&mut conn).await.unwrap()
}

async fn insert_subscription(pool: &database::DbPool, dk: Vec<u8>, gamma: i32) -> i64 {
    use database::schema::subscriptions;
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(subscriptions::table)
        .values((
            subscriptions::detection_key.eq(dk),
            subscriptions::gamma.eq(gamma),
            subscriptions::active.eq(true),
        ))
        .returning(subscriptions::id)
        .get_result(&mut conn)
        .await
        .unwrap()
}

fn dk_bytes_from_dec(scalars: &[&str]) -> Vec<u8> {
    use ark_ed_on_bn254::Fr;
    use std::str::FromStr;
    let mut out = Vec::with_capacity(scalars.len() * 32);
    for s in scalars {
        let fr = Fr::from_str(s).unwrap();
        let mut bytes = fr.into_bigint().to_bytes_le();
        bytes.resize(32, 0);
        out.extend_from_slice(&bytes);
    }
    out
}

fn gamma3_r() -> (U256, U256) {
    let r_packed = hex::decode(GAMMA3_R_PACKED_HEX).unwrap();
    let p = clue::unpack(&r_packed).unwrap();
    (fq_to_u256(p.x), fq_to_u256(p.y))
}

/// Insert a complete tx (RootAdvanced + one `NotePayload` per output leaf)
/// for one chain. The pool emits `RootAdvanced` first, then one note log per
/// inserted leaf, so `inserted` must equal `cms.len()`.
#[allow(clippy::too_many_arguments)]
async fn insert_tx(
    pool: &database::DbPool,
    chain_id: i64,
    start_index: u64,
    cms: &[u8; 2],
    clue_rx: U256,
    clue_ry: U256,
    clue_bits: u16,
    block_n: u64,
    tx_byte: u8,
) {
    insert_log(
        pool,
        chain_id,
        &root_advanced_log(start_index, cms.len() as u64, block_n, tx_byte, 0),
        EventKind::RootAdvanced,
    )
    .await;
    for (i, cm_byte) in cms.iter().enumerate() {
        insert_log(
            pool,
            chain_id,
            &note_payload_log(
                *cm_byte,
                clue_rx,
                clue_ry,
                clue_bits,
                vec![0u8; 8],
                block_n,
                tx_byte,
                1 + i as u64,
            ),
            EventKind::NoteCreated,
        )
        .await;
    }
}

#[tokio::test]
async fn consume_populates_notes_with_leaf_index_per_chain() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;
    insert_chain_state(&pool, CHAIN_B).await;
    let (rx, ry) = gamma3_r();

    insert_tx(
        &pool,
        CHAIN_A,
        0,
        &[0x01, 0x02],
        rx,
        ry,
        GAMMA3_BITS_LE,
        100,
        0x10,
    )
    .await;
    insert_tx(
        &pool,
        CHAIN_A,
        2,
        &[0x03, 0x04],
        rx,
        ry,
        GAMMA3_BITS_LE,
        101,
        0x11,
    )
    .await;
    insert_tx(
        &pool,
        CHAIN_B,
        0,
        &[0x05, 0x06],
        rx,
        ry,
        GAMMA3_BITS_LE,
        200,
        0x20,
    )
    .await;

    let consume = build_consume(&pool);
    let _ = consume.tick_chain(CHAIN_A, 100).await.unwrap();
    let _ = consume.tick_chain(CHAIN_B, 100).await.unwrap();

    assert_eq!(count_notes(&pool, CHAIN_A).await, 4);
    assert_eq!(count_notes(&pool, CHAIN_B).await, 2);

    use database::schema::notes;
    let mut conn = pool.get().await.unwrap();
    let leaves: Vec<i64> = notes::table
        .filter(notes::chain_id.eq(CHAIN_A))
        .order(notes::leaf_index.asc())
        .select(notes::leaf_index)
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(leaves, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn consume_idempotent_replay() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;
    let (rx, ry) = gamma3_r();
    insert_tx(
        &pool,
        CHAIN_A,
        0,
        &[0x01, 0x02],
        rx,
        ry,
        GAMMA3_BITS_LE,
        100,
        0x10,
    )
    .await;

    let consume = build_consume(&pool);
    let _ = consume.tick_chain(CHAIN_A, 100).await.unwrap();
    let n1 = count_notes(&pool, CHAIN_A).await;

    {
        use database::schema::consumer_cursors;
        let mut conn = pool.get().await.unwrap();
        diesel::update(consumer_cursors::table)
            .filter(consumer_cursors::name.eq("fmd"))
            .filter(consumer_cursors::chain_id.eq(CHAIN_A))
            .set(consumer_cursors::last_event_id.eq(0))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let _ = consume.tick_chain(CHAIN_A, 100).await.unwrap();

    assert_eq!(count_notes(&pool, CHAIN_A).await, n1);
}

#[tokio::test]
async fn filter_emits_match_via_test_clue() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    let dk = dk_bytes_from_dec(&GAMMA3_DK);
    let sub_id = insert_subscription(&pool, dk, GAMMA3_GAMMA).await;

    let (rx, ry) = gamma3_r();
    insert_tx(
        &pool,
        CHAIN_A,
        0,
        &[0xbe, 0xef],
        rx,
        ry,
        GAMMA3_BITS_LE,
        100,
        0x10,
    )
    .await;
    let consume = build_consume(&pool);
    let _ = consume.tick_chain(CHAIN_A, 100).await.unwrap();
    assert_eq!(count_notes(&pool, CHAIN_A).await, 2);

    let filter = build_filter(&pool);
    let _ = filter.tick_chain(CHAIN_A, 100).await.unwrap();
    assert_eq!(
        count_matches(&pool).await,
        2,
        "vector gamma=3 must match both notes"
    );

    use database::schema::matches;
    let mut conn = pool.get().await.unwrap();
    let matched_sub: i64 = matches::table
        .select(matches::subscription_id)
        .first(&mut conn)
        .await
        .unwrap();
    assert_eq!(matched_sub, sub_id);

    {
        use database::schema::consumer_cursors;
        diesel::update(consumer_cursors::table)
            .filter(consumer_cursors::name.eq("fmd-filter"))
            .filter(consumer_cursors::chain_id.eq(CHAIN_A))
            .set(consumer_cursors::last_event_id.eq(0))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let _ = filter.tick_chain(CHAIN_A, 100).await.unwrap();
    assert_eq!(count_matches(&pool).await, 2, "ON CONFLICT DO NOTHING");
}

fn new_nf(block_number: i64, log_index: i32, tag: u8) -> NewSpentNullifier {
    NewSpentNullifier {
        chain_id: CHAIN_A,
        block_number,
        log_index,
        nf: vec![tag; 32],
        tx_hash: vec![0xcc; 32],
        block_ts: 1_700_000_000 + block_number,
    }
}

async fn seqs(pool: &database::DbPool, chain_id: i64) -> Vec<i64> {
    use database::schema::spent_nullifiers as sn;
    let mut conn = pool.get().await.unwrap();
    sn::table
        .filter(sn::chain_id.eq(chain_id))
        .order(sn::seq.asc())
        .select(sn::seq)
        .load(&mut conn)
        .await
        .unwrap()
}

#[tokio::test]
async fn spent_nullifier_seq_is_dense_across_batches_and_chains() {
    let (pool, _serial) = fresh_pool().await;
    let repo = PostgresSpentNullifiersRepo::new(pool.clone());

    // Out-of-order within the batch: `seq` must follow (block_number,
    // log_index), not the order the caller happened to build the rows in.
    repo.insert_batch(&[new_nf(10, 1, 0xb1), new_nf(10, 0, 0xb0)])
        .await
        .unwrap();
    repo.insert_batch(&[new_nf(11, 0, 0xb2)]).await.unwrap();

    assert_eq!(seqs(&pool, CHAIN_A).await, vec![0, 1, 2]);

    let other = NewSpentNullifier {
        chain_id: CHAIN_B,
        ..new_nf(10, 0, 0xc0)
    };
    PostgresSpentNullifiersRepo::new(pool.clone())
        .insert_batch(&[other])
        .await
        .unwrap();
    assert_eq!(seqs(&pool, CHAIN_B).await, vec![0], "seq is per chain");

    use database::schema::spent_nullifiers as sn;
    let mut conn = pool.get().await.unwrap();
    let ordered: Vec<i64> = sn::table
        .filter(sn::chain_id.eq(CHAIN_A))
        .order(sn::seq.asc())
        .select(sn::block_number)
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(ordered, vec![10, 10, 11]);
}

#[tokio::test]
async fn spent_nullifier_seq_survives_replay_and_reorg() {
    let (pool, _serial) = fresh_pool().await;
    let repo = PostgresSpentNullifiersRepo::new(pool.clone());

    let batch = [new_nf(10, 0, 0xb0), new_nf(11, 0, 0xb1)];
    repo.insert_batch(&batch).await.unwrap();

    // Crash between insert and cursor upsert replays the same batch. Rows
    // already stored must not consume ordinals, or the next insert collides.
    assert_eq!(repo.insert_batch(&batch).await.unwrap(), 0);
    repo.insert_batch(&[new_nf(12, 0, 0xb2)]).await.unwrap();
    assert_eq!(seqs(&pool, CHAIN_A).await, vec![0, 1, 2]);

    // Reorg trims the tail; the sequence stays dense and re-extends from
    // the surviving max, so completed chunks keep their contents.
    repo.delete_from_block(CHAIN_A, 11).await.unwrap();
    assert_eq!(seqs(&pool, CHAIN_A).await, vec![0]);
    repo.insert_batch(&[new_nf(11, 0, 0xd1), new_nf(12, 0, 0xd2)])
        .await
        .unwrap();
    assert_eq!(seqs(&pool, CHAIN_A).await, vec![0, 1, 2]);
}

// --------------------------------------------------------- replica failover

const NS_OTHER: i64 = database::advisory::NS_INGESTER;

async fn cursor_of(pool: &database::DbPool, name: &str, chain_id: i64) -> i64 {
    PostgresCursorRepo::new(pool.clone())
        .fetch(name, chain_id)
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn chain_lock_is_exclusive_and_releases_on_drop() {
    let (_pool, _serial) = fresh_pool().await;
    let url = db_url().await;
    let key = database::advisory::chain_key(database::advisory::NS_FMD_CONSUME, CHAIN_A);

    let first = ChainLock::try_acquire(url, key).await.unwrap();
    assert!(first.is_some(), "uncontended acquire must win");
    assert!(
        ChainLock::try_acquire(url, key).await.unwrap().is_none(),
        "second holder must be turned away, not blocked"
    );

    // Failover: the leader dying frees the lock for a standby.
    drop(first);
    assert!(
        ChainLock::try_acquire(url, key).await.unwrap().is_some(),
        "lock must release when the holder drops"
    );
}

#[tokio::test]
async fn chain_locks_are_scoped_by_chain_and_namespace() {
    let (_pool, _serial) = fresh_pool().await;
    let url = db_url().await;
    let ns = database::advisory::NS_FMD_CONSUME;

    let _a = ChainLock::try_acquire(url, database::advisory::chain_key(ns, CHAIN_A))
        .await
        .unwrap()
        .expect("chain A");
    assert!(
        ChainLock::try_acquire(url, database::advisory::chain_key(ns, CHAIN_B))
            .await
            .unwrap()
            .is_some(),
        "a lock on one chain must not block another chain"
    );
    // fmd-indexer and ingester guard different tables; neither may exclude the
    // other for the same chain.
    assert!(
        ChainLock::try_acquire(url, database::advisory::chain_key(NS_OTHER, CHAIN_A))
            .await
            .unwrap()
            .is_some(),
        "namespaces must not collide"
    );
}

/// A freshly acquired lock must report itself alive.
///
/// `is_leader` demotes on `!is_alive()`, so a check that cannot succeed makes
/// every holder release the lock it just took: fmd-indexer flaps between
/// leader and standby on alternate ticks, and ingester — which stops for good
/// on lock loss — halts after its first poll. The other lock tests all take a
/// lock and inspect it from *another* connection, so none of them touch this
/// path.
#[tokio::test]
async fn a_held_lock_reports_itself_alive() {
    let (_pool, _serial) = fresh_pool().await;
    let url = db_url().await;
    let key = database::advisory::chain_key(database::advisory::NS_FMD_CONSUME, CHAIN_A);

    let mut lock = ChainLock::try_acquire(url, key)
        .await
        .unwrap()
        .expect("uncontended lock");
    assert!(
        lock.is_alive().await,
        "a live session must not read as dead"
    );
    // Idempotent: the tick loop calls this on every pass.
    assert!(lock.is_alive().await, "still alive on a second check");
}

/// The leadership loop over a real connection: acquire, stay leader across
/// consecutive ticks, and only demote once the lock is genuinely gone.
#[tokio::test]
async fn leadership_survives_consecutive_ticks() {
    let (_pool, _serial) = fresh_pool().await;
    let url = db_url().await;
    let locks = ChainLocks::enabled(url);

    assert!(
        locks.is_leader(CHAIN_A).await.unwrap(),
        "first tick acquires"
    );
    for tick in 2..=5 {
        assert!(
            locks.is_leader(CHAIN_A).await.unwrap(),
            "tick {tick} must keep leadership, not flap to standby"
        );
    }
}

#[tokio::test]
async fn standby_replica_does_no_work_until_the_leader_releases() {
    let (pool, _serial) = fresh_pool().await;
    let url = db_url().await;
    insert_chain_state(&pool, CHAIN_A).await;
    let (rx, ry) = gamma3_r();
    insert_tx(
        &pool,
        CHAIN_A,
        0,
        &[0x01, 0x02],
        rx,
        ry,
        GAMMA3_BITS_LE,
        100,
        0x10,
    )
    .await;

    let standby = ConsumeServiceImpl::new(
        pool.clone(),
        Arc::new(PostgresCursorRepo::new(pool.clone())),
        Arc::new(PostgresRawEventsRepo::new(pool.clone())),
        Arc::new(PostgresNotesRepo::new(pool.clone())),
        Arc::new(PostgresSpentNullifiersRepo::new(pool.clone())),
        ChainLocks::enabled(url),
    );

    // Another replica is the leader.
    let leader = ChainLock::try_acquire(
        url,
        database::advisory::chain_key(database::advisory::NS_FMD_CONSUME, CHAIN_A),
    )
    .await
    .unwrap()
    .expect("leader lock");

    let _ = standby.tick_chain(CHAIN_A, 100).await.unwrap();
    assert_eq!(
        count_notes(&pool, CHAIN_A).await,
        0,
        "standby must not write"
    );
    assert_eq!(
        cursor_of(&pool, "fmd", CHAIN_A).await,
        0,
        "cursor untouched"
    );

    // Leader dies; the standby is promoted on its next tick.
    drop(leader);
    let _ = standby.tick_chain(CHAIN_A, 100).await.unwrap();
    assert_eq!(
        count_notes(&pool, CHAIN_A).await,
        2,
        "promoted and caught up"
    );
    assert!(cursor_of(&pool, "fmd", CHAIN_A).await > 0);
}

#[tokio::test]
async fn concurrent_replicas_keep_spent_nullifier_seq_dense() {
    let (pool, _serial) = fresh_pool().await;
    let url = db_url().await;
    insert_chain_state(&pool, CHAIN_A).await;
    let (rx, ry) = gamma3_r();
    for (i, tx_byte) in [0x10u8, 0x11, 0x12].iter().enumerate() {
        insert_tx(
            &pool,
            CHAIN_A,
            (i * 2) as u64,
            &[0x20 + i as u8, 0x30 + i as u8],
            rx,
            ry,
            GAMMA3_BITS_LE,
            100 + i as u64,
            *tx_byte,
        )
        .await;
    }

    let build = || {
        ConsumeServiceImpl::new(
            pool.clone(),
            Arc::new(PostgresCursorRepo::new(pool.clone())),
            Arc::new(PostgresRawEventsRepo::new(pool.clone())),
            Arc::new(PostgresNotesRepo::new(pool.clone())),
            Arc::new(PostgresSpentNullifiersRepo::new(pool.clone())),
            ChainLocks::enabled(url),
        )
    };
    let (a, b) = (build(), build());
    let (ra, rb) = tokio::join!(a.tick_chain(CHAIN_A, 100), b.tick_chain(CHAIN_A, 100));
    let _ = ra.unwrap();
    let _ = rb.unwrap();

    assert_eq!(count_notes(&pool, CHAIN_A).await, 6);
    // `seq` is assigned by a read-then-write with no transaction; a second
    // concurrent writer would leave permanent gaps.
    let seqs = seqs(&pool, CHAIN_A).await;
    assert_eq!(
        seqs,
        (0..seqs.len() as i64).collect::<Vec<_>>(),
        "seq must stay gapless"
    );
}

#[tokio::test]
async fn cursor_advance_is_monotonic_but_reset_still_rewinds() {
    let (pool, _serial) = fresh_pool().await;
    let repo = PostgresCursorRepo::new(pool.clone());
    let row = |last_event_id: i64| UpsertCursor {
        name: "fmd".to_string(),
        chain_id: CHAIN_A,
        last_event_id,
        last_block_number: last_event_id,
    };

    repo.upsert_monotonic(row(100)).await.unwrap();
    assert_eq!(cursor_of(&pool, "fmd", CHAIN_A).await, 100);

    // A slower replica landing late must not drag the watermark backwards.
    repo.upsert_monotonic(row(50)).await.unwrap();
    assert_eq!(cursor_of(&pool, "fmd", CHAIN_A).await, 100, "no regression");

    repo.upsert_monotonic(row(150)).await.unwrap();
    assert_eq!(cursor_of(&pool, "fmd", CHAIN_A).await, 150, "advances");

    // The reset path is a deliberate rewind and keeps using plain `upsert`.
    repo.upsert(row(0)).await.unwrap();
    assert_eq!(
        cursor_of(&pool, "fmd", CHAIN_A).await,
        0,
        "reset still works"
    );
}

// ---------------------------------------------------------------------------
// Regressions for the silent-stall and silent-skip paths.
//
// Every one of these used to end with the loop returning `Ok(())` forever
// while the cursor stood still, or with a note quietly never scanned. They all
// assert on progress, not on an error, because progress is exactly what the
// old code stopped making without saying so.
// ---------------------------------------------------------------------------

fn nullifier_consumed_log(nf_byte: u8, block_n: u64, tx_byte: u8, log_idx: u64) -> Log {
    let ev = NullifierConsumed {
        nf: B256::repeat_byte(nf_byte),
    };
    build_log(ev.encode_log_data(), block_n, tx_byte, log_idx)
}

async fn cursor_block_of(pool: &database::DbPool, name: &str, chain_id: i64) -> i64 {
    PostgresCursorRepo::new(pool.clone())
        .fetch(name, chain_id)
        .await
        .unwrap()
        .1
}

async fn leaf_indices(pool: &database::DbPool, chain_id: i64) -> Vec<i64> {
    use database::schema::notes;
    let mut conn = pool.get().await.unwrap();
    notes::table
        .filter(notes::chain_id.eq(chain_id))
        .order(notes::leaf_index.asc())
        .select(notes::leaf_index)
        .load(&mut conn)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_unusable_leaf_leaves_a_hole_instead_of_wedging_the_chain() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;
    let (rx, ry) = gamma3_r();

    // Two leaves announced; the second carries a ciphertext too short to hold
    // the clueBits prefix, so it can never become a note. Completion used to
    // be `notes.len() == inserted`, which this tx could never satisfy — the
    // cursor parked on it and the chain stopped for good.
    insert_log(
        &pool,
        CHAIN_A,
        &root_advanced_log(0, 2, 100, 0x51, 0),
        EventKind::RootAdvanced,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &note_payload_log(0x60, rx, ry, GAMMA3_BITS_LE, vec![0u8; 8], 100, 0x51, 1),
        EventKind::NoteCreated,
    )
    .await;
    let truncated = note_payload_log_raw(0x61, rx, ry, vec![0x00], 100, 0x51, 2);
    insert_log(&pool, CHAIN_A, &truncated, EventKind::NoteCreated).await;

    // A later, wholly valid tx: it must not be held hostage by the first.
    insert_tx(
        &pool,
        CHAIN_A,
        2,
        &[0x70, 0x71],
        rx,
        ry,
        GAMMA3_BITS_LE,
        101,
        0x52,
    )
    .await;

    let consume = build_consume(&pool);
    let _ = consume.tick_chain(CHAIN_A, 100).await.unwrap();

    assert_eq!(
        count_notes(&pool, CHAIN_A).await,
        3,
        "the usable leaves land"
    );
    // Leaf 1 is the hole. Surviving leaves keep their true indices — the
    // ordinal is taken from the event's position, not from how many notes
    // happened to be pushed before it.
    assert_eq!(leaf_indices(&pool, CHAIN_A).await, vec![0, 2, 3]);
    assert!(
        cursor_of(&pool, "fmd", CHAIN_A).await > 0,
        "cursor must advance past a tx it can never fully decode"
    );
}

#[tokio::test]
async fn a_tx_wider_than_the_batch_still_commits() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;
    let (rx, ry) = gamma3_r();

    // 1 root + 6 notes = 7 rows, fetched two at a time. The head tx can never
    // fit the window, and re-ticking re-reads the same two rows, so the old
    // code made no progress on any batch size below 7.
    insert_log(
        &pool,
        CHAIN_A,
        &root_advanced_log(0, 6, 100, 0x53, 0),
        EventKind::RootAdvanced,
    )
    .await;
    for i in 0..6u8 {
        insert_log(
            &pool,
            CHAIN_A,
            &note_payload_log(
                0x80 + i,
                rx,
                ry,
                GAMMA3_BITS_LE,
                vec![0u8; 8],
                100,
                0x53,
                1 + i as u64,
            ),
            EventKind::NoteCreated,
        )
        .await;
    }

    let consume = build_consume(&pool);
    let _ = consume.tick_chain(CHAIN_A, 2).await.unwrap();

    assert_eq!(
        count_notes(&pool, CHAIN_A).await,
        6,
        "window widened to fit"
    );
    assert_eq!(leaf_indices(&pool, CHAIN_A).await, vec![0, 1, 2, 3, 4, 5]);
    assert!(cursor_of(&pool, "fmd", CHAIN_A).await > 0);
}

#[tokio::test]
async fn a_second_root_advanced_in_one_tx_is_refused() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;
    let (rx, ry) = gamma3_r();

    // Two roots in one tx: the second used to renumber the first root's leaf
    // onto its own start_index, silently writing indices that belong to
    // another range (and colliding on `notes_chain_leaf_idx`). Failing the
    // tick is the only outcome that is neither wrong nor silent.
    insert_log(
        &pool,
        CHAIN_A,
        &root_advanced_log(0, 1, 100, 0x54, 0),
        EventKind::RootAdvanced,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &note_payload_log(0x90, rx, ry, GAMMA3_BITS_LE, vec![0u8; 8], 100, 0x54, 1),
        EventKind::NoteCreated,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &root_advanced_log(64, 1, 100, 0x54, 2),
        EventKind::RootAdvanced,
    )
    .await;

    let consume = build_consume(&pool);
    let err = consume.tick_chain(CHAIN_A, 100).await.unwrap_err();
    assert!(
        err.to_string().contains("RootAdvanced"),
        "must name the violated invariant, got: {err}"
    );
    assert_eq!(count_notes(&pool, CHAIN_A).await, 0, "nothing written");
}

#[tokio::test]
async fn a_spend_only_tx_commits_and_keeps_the_cursor_block() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    // A tx that burns nullifiers and creates no leaf has no RootAdvanced, so
    // it never satisfied `root_seen` and blocked the chain outright. And the
    // committed block came from the last *note*, so a batch ending on one of
    // these rewound `last_block_number` to 0.
    insert_log(
        &pool,
        CHAIN_A,
        &nullifier_consumed_log(0xa1, 500, 0x55, 0),
        EventKind::NullifierConsumed,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &nullifier_consumed_log(0xa2, 500, 0x55, 1),
        EventKind::NullifierConsumed,
    )
    .await;

    let consume = build_consume(&pool);
    let _ = consume.tick_chain(CHAIN_A, 100).await.unwrap();

    assert_eq!(seqs(&pool, CHAIN_A).await, vec![0, 1], "nullifiers indexed");
    assert!(
        cursor_of(&pool, "fmd", CHAIN_A).await > 0,
        "cursor advances"
    );
    assert_eq!(
        cursor_block_of(&pool, "fmd", CHAIN_A).await,
        500,
        "block must come from the tx, not only from its notes"
    );
}

#[tokio::test]
async fn spent_nullifier_dedup_survives_the_bounded_range_read() {
    let (pool, _serial) = fresh_pool().await;
    let repo = PostgresSpentNullifiersRepo::new(pool.clone());

    repo.insert_batch(&[new_nf(10, 0, 0xd0)]).await.unwrap();
    repo.insert_batch(&[new_nf(20, 0, 0xd1)]).await.unwrap();

    // The dedup read is now bounded above by the batch's own max block, so it
    // no longer drags in every later row. The block-10 replay must still see
    // itself as stored, or it would burn an ordinal and gap the sequence.
    assert_eq!(repo.insert_batch(&[new_nf(10, 0, 0xd0)]).await.unwrap(), 0);
    repo.insert_batch(&[new_nf(30, 0, 0xd2)]).await.unwrap();
    assert_eq!(seqs(&pool, CHAIN_A).await, vec![0, 1, 2]);
}

#[tokio::test]
async fn backfill_will_not_page_past_freshly_written_notes() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;
    let (rx, ry) = gamma3_r();
    insert_tx(
        &pool,
        CHAIN_A,
        0,
        &[0xb1, 0xb2],
        rx,
        ry,
        GAMMA3_BITS_LE,
        100,
        0x56,
    )
    .await;
    let _ = build_consume(&pool).tick_chain(CHAIN_A, 100).await.unwrap();

    let sub_id = insert_subscription(&pool, dk_bytes_from_dec(&GAMMA3_DK), GAMMA3_GAMMA).await;
    let subs = PostgresSubscriptionsRepo::new(pool.clone());
    let filter = build_filter(&pool);

    // `notes.id` is allocated before commit, so `max(id)` can already have
    // skipped an in-flight row belonging to another chain's leader. The head
    // is therefore an id that was visible a lag ago — which, for notes written
    // this instant, is no id at all. Advancing here would mark the
    // subscription caught up on rows it never scanned.
    for _ in 0..3 {
        let _ = filter.tick_chain(CHAIN_A, 100).await.unwrap();
    }
    let pointer = subs
        .next_backfilling(i64::MAX)
        .await
        .unwrap()
        .expect("subscription still behind")
        .backfilled_through_note_id;
    assert_eq!(
        pointer, 0,
        "backfill must not claim notes newer than the safe watermark"
    );

    // The forward pass is per-chain and gap-safe, so it is unaffected: the
    // notes are matched there, on this tick, without waiting for the lag.
    assert_eq!(count_matches(&pool).await, 2);
    let _ = sub_id;
}

#[tokio::test]
async fn advance_backfill_never_rewinds() {
    let (pool, _serial) = fresh_pool().await;
    let sub_id = insert_subscription(&pool, dk_bytes_from_dec(&GAMMA3_DK), GAMMA3_GAMMA).await;
    let subs = PostgresSubscriptionsRepo::new(pool.clone());

    let pointer = || async {
        subs.next_backfilling(i64::MAX)
            .await
            .unwrap()
            .map(|s| s.backfilled_through_note_id)
    };

    subs.advance_backfill(sub_id, 500).await.unwrap();
    assert_eq!(pointer().await, Some(500));

    // Two unlocked replicas race; the slower one's older pointer must not
    // undo the faster one's progress.
    subs.advance_backfill(sub_id, 200).await.unwrap();
    assert_eq!(pointer().await, Some(500), "no regression");

    subs.advance_backfill(sub_id, 900).await.unwrap();
    assert_eq!(pointer().await, Some(900), "advances");
}
