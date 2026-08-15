//! Lazy-root v2 integration test.
//!
//! Drives the indexer halves (fmd + explorer) over synthetic raw_events
//! representing one MASP `transact()` call:
//!
//!   tx_hash T:
//!     log_index 0  RootAdvanced(start_index=0, inserted=2, oldRoot, newRoot)
//!     log_index 1  NotesCreated(cm0, cm1, …, ciphertext0, …, ciphertext1)
//!
//! Asserts:
//!   - fmd-indexer consume populates `notes` with leaf_index ∈ {0, 1}.
//!   - explorer-indexer consume populates `tree_advances` with start_index=0.
//!
//! Anvil + relayer + snarkjs not booted here — those need node + circuits
//! build artifacts and live in `cargo run -p relayer` against a real chain.

use alloy::primitives::{Address, B256, LogData, U256};
use alloy::sol_types::SolEvent;
use chain_types::abi::{AssetRegistered, NotePayload, RootAdvanced};
use database::schema::{notes, raw_events, tree_advances};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use shared::entities::EventKind;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

const CHAIN_ID: i64 = 31337;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fmd_consume_pairs_root_advanced_with_note_created() {
    let _guard = serial_lock().await;
    let pool = boot_pool().await;
    seed_chain_state(&pool, CHAIN_ID).await;

    let cm0 = B256::repeat_byte(0xa0);
    let cm1 = B256::repeat_byte(0xa1);
    let old_root = B256::repeat_byte(0x00);
    let new_root = B256::repeat_byte(0xbb);
    let tx = vec![0xde; 32];

    insert_root_advanced_event(
        &pool, CHAIN_ID, 100, 1700000000, &tx, 0, 0, 2, old_root, new_root,
    )
    .await;
    // One `NotePayload` log per output leaf, in leaf order.
    insert_note_payload_event(&pool, CHAIN_ID, 100, 1700000000, &tx, 1, cm0).await;
    insert_note_payload_event(&pool, CHAIN_ID, 100, 1700000000, &tx, 2, cm1).await;

    let cursors = Arc::new(fmd_indexer::repositories::cursor::PostgresCursorRepo::new(
        pool.clone(),
    ));
    let raw_events_repo =
        Arc::new(fmd_indexer::repositories::raw_events::PostgresRawEventsRepo::new(pool.clone()));
    let notes_repo = Arc::new(fmd_indexer::repositories::notes::PostgresNotesRepo::new(
        pool.clone(),
    ));
    let spent_nfs_repo = Arc::new(
        fmd_indexer::repositories::spent_nullifiers::PostgresSpentNullifiersRepo::new(pool.clone()),
    );
    let svc = fmd_indexer::services::consume::ConsumeServiceImpl::new(
        cursors,
        raw_events_repo,
        notes_repo,
        spent_nfs_repo,
        fmd_indexer::adapters::locks::ChainLocks::disabled(),
    );
    use fmd_indexer::services::consume::ConsumeService;
    svc.tick_chain(CHAIN_ID, 100)
        .await
        .expect("fmd consume tick");

    let mut conn = pool.get().await.unwrap();
    let rows: Vec<(Vec<u8>, i64)> = notes::table
        .filter(notes::chain_id.eq(CHAIN_ID))
        .order(notes::leaf_index.asc())
        .select((notes::cm, notes::leaf_index))
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, cm0.0.to_vec());
    assert_eq!(rows[0].1, 0);
    assert_eq!(rows[1].0, cm1.0.to_vec());
    assert_eq!(rows[1].1, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explorer_consume_writes_tree_advances() {
    let _guard = serial_lock().await;
    let pool = boot_pool().await;
    seed_chain_state(&pool, CHAIN_ID).await;

    let old_root = B256::repeat_byte(0x00);
    let new_root = B256::repeat_byte(0xcc);
    let tx = vec![0xab; 32];

    insert_root_advanced_event(
        &pool, CHAIN_ID, 200, 1700000100, &tx, 0, 0, 2, old_root, new_root,
    )
    .await;
    insert_asset_registered_event(&pool, CHAIN_ID, 200, 1700000100, &tx, 1, 1).await;

    let cfg = Arc::new(explorer_indexer::config::ExplorerIndexerConfig {
        database_url: String::new(),
        tick_ms: 1000,
        batch: 500,
    });
    let ctx = explorer_indexer::services::consume::ConsumeCtx {
        pool: pool.clone(),
        cfg,
    };
    explorer_indexer::services::consume::tick_chain(&ctx, CHAIN_ID, 100)
        .await
        .expect("explorer consume tick");

    let mut conn = pool.get().await.unwrap();
    let advances: Vec<(i64, i32, Vec<u8>, Vec<u8>)> = tree_advances::table
        .filter(tree_advances::chain_id.eq(CHAIN_ID))
        .select((
            tree_advances::start_index,
            tree_advances::inserted,
            tree_advances::old_root,
            tree_advances::new_root,
        ))
        .load(&mut conn)
        .await
        .unwrap();
    assert_eq!(advances.len(), 1);
    let (start_index, inserted, old, new) = &advances[0];
    assert_eq!(*start_index, 0);
    assert_eq!(*inserted, 2);
    assert_eq!(old, &old_root.0.to_vec());
    assert_eq!(new, &new_root.0.to_vec());
}

// ------------------------------------------------------------------ helpers

async fn serial_lock() -> tokio::sync::OwnedMutexGuard<()> {
    use std::sync::OnceLock;
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

async fn boot_pool() -> database::DbPool {
    static CONTAINER: tokio::sync::OnceCell<(testcontainers::ContainerAsync<Postgres>, String)> =
        tokio::sync::OnceCell::const_new();
    let (_container, url) = CONTAINER
        .get_or_init(|| async {
            let container = Postgres::default().start().await.unwrap();
            let port = container.get_host_port_ipv4(5432).await.unwrap();
            let url = format!("postgres://postgres:postgres@localhost:{}/postgres", port);
            for _ in 0..30 {
                if database::build_pool(&url, database::PoolCfg::relayer())
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let migrate_url = url.clone();
            tokio::task::spawn_blocking(move || database::migrate::run(&migrate_url))
                .await
                .expect("blocking")
                .expect("migrate");
            (container, url)
        })
        .await;

    let pool = database::build_pool(url, database::PoolCfg::relayer())
        .await
        .expect("pool");
    truncate_all(&pool).await;
    pool
}

async fn truncate_all(pool: &database::DbPool) {
    let mut conn = pool.get().await.unwrap();
    diesel::sql_query("TRUNCATE TABLE notes, raw_events, tree_advances, consumer_cursors, chain_state, assets, matches, subscriptions, spent_nullifiers RESTART IDENTITY CASCADE")
        .execute(&mut conn)
        .await
        .ok();
}

async fn seed_chain_state(pool: &database::DbPool, chain_id: i64) {
    use database::schema::{chain_state, consumer_cursors};
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(chain_state::table)
        .values((
            chain_state::chain_id.eq(chain_id),
            chain_state::last_block.eq(0i64),
            chain_state::last_block_hash.eq(vec![0u8; 32]),
            chain_state::last_scanned_block.eq(0i64),
        ))
        .on_conflict(chain_state::chain_id)
        .do_nothing()
        .execute(&mut conn)
        .await
        .unwrap();

    for name in &["fmd", "explorer"] {
        diesel::insert_into(consumer_cursors::table)
            .values((
                consumer_cursors::name.eq(*name),
                consumer_cursors::chain_id.eq(chain_id),
                consumer_cursors::last_event_id.eq(0i64),
                consumer_cursors::last_block_number.eq(0i64),
            ))
            .on_conflict((consumer_cursors::name, consumer_cursors::chain_id))
            .do_nothing()
            .execute(&mut conn)
            .await
            .unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_raw_event(
    pool: &database::DbPool,
    chain_id: i64,
    block_number: i64,
    block_ts: i64,
    tx_hash: &[u8],
    log_index: i32,
    kind: EventKind,
    log: LogData,
) {
    let topics: Vec<Vec<u8>> = log.topics().iter().map(|t| t.0.to_vec()).collect();
    let data = log.data.to_vec();
    let mut conn = pool.get().await.unwrap();
    diesel::insert_into(raw_events::table)
        .values((
            raw_events::chain_id.eq(chain_id),
            raw_events::block_number.eq(block_number),
            raw_events::block_hash.eq(vec![0u8; 32]),
            raw_events::block_ts.eq(block_ts),
            raw_events::tx_hash.eq(tx_hash.to_vec()),
            raw_events::log_index.eq(log_index),
            raw_events::event_kind.eq(kind.as_i16()),
            raw_events::topics.eq(topics),
            raw_events::data.eq(data),
        ))
        .execute(&mut conn)
        .await
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
async fn insert_root_advanced_event(
    pool: &database::DbPool,
    chain_id: i64,
    block_number: i64,
    block_ts: i64,
    tx_hash: &[u8],
    log_index: i32,
    start_index: u64,
    inserted: u64,
    old_root: B256,
    new_root: B256,
) {
    let ev = RootAdvanced {
        startIndex: start_index,
        inserted,
        oldRoot: old_root,
        newRoot: new_root,
    };
    let log = ev.encode_log_data();
    insert_raw_event(
        pool,
        chain_id,
        block_number,
        block_ts,
        tx_hash,
        log_index,
        EventKind::RootAdvanced,
        log,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn insert_note_payload_event(
    pool: &database::DbPool,
    chain_id: i64,
    block_number: i64,
    block_ts: i64,
    tx_hash: &[u8],
    log_index: i32,
    cm: B256,
) {
    // 2-byte clueBits prefix + 4-byte body.
    let ciphertext: alloy::primitives::Bytes = vec![0x00, 0x00, 0xde, 0xad, 0xbe, 0xef].into();
    let ev = NotePayload {
        cm,
        clueRx: U256::from(0u64),
        clueRy: U256::from(0u64),
        ephPubX: U256::from(0u64),
        ephPubY: U256::from(0u64),
        ciphertext,
        cvDepX: U256::from(0u64),
        cvDepY: U256::from(0u64),
    };
    let log = ev.encode_log_data();
    insert_raw_event(
        pool,
        chain_id,
        block_number,
        block_ts,
        tx_hash,
        log_index,
        EventKind::NoteCreated,
        log,
    )
    .await;
}

async fn insert_asset_registered_event(
    pool: &database::DbPool,
    chain_id: i64,
    block_number: i64,
    block_ts: i64,
    tx_hash: &[u8],
    log_index: i32,
    asset_id: u64,
) {
    let ev = AssetRegistered {
        assetId: asset_id,
        token: Address::repeat_byte(0xee),
        scale: U256::from(1u64),
    };
    let log = ev.encode_log_data();
    insert_raw_event(
        pool,
        chain_id,
        block_number,
        block_ts,
        tx_hash,
        log_index,
        EventKind::AssetRegistered,
        log,
    )
    .await;
}

/// The spent set is served as a chunk feed
/// so wallets filter locally instead of telling the server which
/// nullifiers they hold. Mirrors the commitment feed's slicing and
/// cache semantics.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nullifier_chunk_feed_slices_spent_set() {
    use fmd_indexer::repositories::spent_nullifiers::{
        NewSpentNullifier, PostgresSpentNullifiersRepo, SpentNullifiersRepo,
    };

    let _guard = serial_lock().await;
    let pool = boot_pool().await;

    // One past a chunk boundary, so chunk 0 is complete and chunk 1 is the tail.
    const TOTAL: usize = 1025;
    let repo = PostgresSpentNullifiersRepo::new(pool.clone());
    let rows: Vec<NewSpentNullifier> = (0..TOTAL)
        .map(|i| {
            let mut nf = vec![0u8; 32];
            nf[24..].copy_from_slice(&(i as u64).to_be_bytes());
            NewSpentNullifier {
                chain_id: CHAIN_ID,
                block_number: i as i64,
                log_index: 0,
                nf,
                tx_hash: vec![0xcc; 32],
                block_ts: 1_700_000_000,
            }
        })
        .collect();
    repo.insert_batch(&rows).await.unwrap();

    let base = spawn_fmd_webserver(&pool).await;
    let client = reqwest::Client::new();

    let fetch = |id: u64| {
        let client = client.clone();
        let url = format!("{base}/v1/chains/{CHAIN_ID}/nullifiers/chunks/{id}");
        async move { client.get(url).send().await.unwrap() }
    };

    let resp = fetch(0).await;
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable"),
        "a full chunk can never change, so it is served as immutable"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["isComplete"], true);
    let first = body["nullifiers"].as_array().unwrap();
    assert_eq!(first.len(), 1024);
    assert_eq!(first[0], format!("0x{}", hex::encode(&rows[0].nf)));
    assert_eq!(first[1023], format!("0x{}", hex::encode(&rows[1023].nf)));

    let resp = fetch(1).await;
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=5"),
        "the tail chunk still grows"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["isComplete"], false);
    let tail = body["nullifiers"].as_array().unwrap();
    assert_eq!(tail.len(), TOTAL - 1024);
    assert_eq!(tail[0], format!("0x{}", hex::encode(&rows[1024].nf)));

    // Past the end: empty, not an error — the client has already stopped.
    let body: serde_json::Value = fetch(2).await.json().await.unwrap();
    assert_eq!(body["nullifiers"].as_array().unwrap().len(), 0);
    assert_eq!(body["isComplete"], false);
}

async fn spawn_fmd_webserver(pool: &database::DbPool) -> String {
    let state = fmd_webserver::AppState {
        pool: pool.clone(),
        cfg: Arc::new(fmd_webserver::FmdWebserverConfig {
            database_url: String::new(),
            bind_addr: String::new(),
            indexer_lag_warn_blocks: 50,
        }),
        cache: fmd_webserver::app::cache::AppCache::new(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = fmd_webserver::build_router(state);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// The chunk feed serves one pre-hashed leaf per entry, as `0x`-prefixed hex.
///
/// Two properties, both load-bearing. The prefix is not cosmetic: the SDK
/// decodes field elements with a helper that accepts decimal *or* `0x`-hex, so
/// a bare-hex value whose digits all happen to be decimal would parse as a
/// completely different number, silently.
///
/// And the raw inputs must be gone. `cm`/`cv_dep` existed only for clients to
/// hash into the leaf themselves; serving them alongside `leafHash` would be
/// three field elements where one does, tripling the largest feed in a cold
/// sync for data nothing reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commitment_chunk_serves_only_a_prefixed_hex_leaf_hash() {
    use bigdecimal::BigDecimal;
    use fmd_indexer::repositories::notes::{NewNote, NotesRepo, PostgresNotesRepo};
    use std::str::FromStr;

    let _guard = serial_lock().await;
    let pool = boot_pool().await;

    // A value whose hex form is all decimal digits — the exact input that a
    // bare-hex wire format would mis-decode on the client.
    // 305419896 == 0x12345678, and "12345678" is also a valid decimal literal
    // for a different number entirely.
    let cv_x = BigDecimal::from_str("305419896").unwrap();
    // A full-width element, to pin the zero-padding and the 64-char width.
    let cv_y = BigDecimal::from_str(
        "21888242871839275222246405745257275088548364400416034343698204186575808495616",
    )
    .unwrap();

    let repo = PostgresNotesRepo::new(pool.clone());
    repo.insert_batch(&[NewNote {
        chain_id: CHAIN_ID,
        block_number: 1,
        tx_hash: vec![0xaa; 32],
        log_index: 0,
        cm: vec![0xbb; 32],
        clue_rx: BigDecimal::from(0),
        clue_ry: BigDecimal::from(0),
        eph_pub_x: BigDecimal::from(7),
        eph_pub_y: BigDecimal::from(0),
        ciphertext: vec![0x00, 0x07],
        leaf_index: 0,
        cv_dep_x: cv_x,
        cv_dep_y: cv_y,
    }])
    .await
    .unwrap();

    let base = spawn_fmd_webserver(&pool).await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/v1/chains/{CHAIN_ID}/commitments/chunks/0"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let entry = &body["entries"][0];
    let leaf = entry["leafHash"].as_str().expect("leafHash is served");
    assert!(leaf.starts_with("0x"), "must be 0x-prefixed: {leaf}");
    assert_eq!(leaf.len(), 66, "0x + 64 hex chars, left-padded: {leaf}");
    assert!(
        leaf[2..].chars().all(|c| c.is_ascii_hexdigit()),
        "must be hex: {leaf}"
    );

    // The inputs the client used to hash itself are no longer on the wire.
    for gone in ["cmHex", "cvDepX", "cvDepY"] {
        assert!(entry.get(gone).is_none(), "{gone} should no longer be served");
    }
}
