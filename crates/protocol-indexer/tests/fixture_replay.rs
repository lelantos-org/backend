//! Fixture-replay tests for protocol-indexer against the `AssetRegistered` and
//! `RootAdvanced` events.

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::eth::Log;
use alloy::sol_types::SolEvent;
use bigdecimal::BigDecimal;
use chain_types::abi::{
    AssetFeeSet, AssetRegistered, DepositEscrowed, DepositFlushed, RootAdvanced,
};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use protocol_indexer::services::consume::{ConsumeCtx, RefreshGate, tick_chain};
use shared::entities::EventKind;
use std::str::FromStr;
use std::sync::Arc;
use test_support::fixtures::{build_log, insert_chain_state, insert_log};

const CHAIN_A: i64 = 1;
const ASSET_ID: u64 = 7;
/// The deposit fixture pays its fee note in another asset than `ASSET_ID`.
const FEE_ASSET_ID: u64 = 9;

/// Cleared between tests. Every table this binary writes, directly or through a
/// foreign key.
const TABLES: &[&str] = &[
    "raw_events",
    "chain_state",
    "chain_reorgs",
    "consumer_cursors",
    "notes",
    "subscriptions",
    "matches",
    "assets",
    "asset_yield",
    "tree_advances",
    "deposit_escrowed_events",
];

async fn fresh_pool() -> (database::DbPool, tokio::sync::OwnedMutexGuard<()>) {
    test_support::fresh_pool(database::PoolCfg::indexer(), TABLES).await
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

fn asset_fee_set_log(
    asset_id: u64,
    deposit_bps: u16,
    withdraw_bps: u16,
    block_n: u64,
    block_ts: u64,
    tx_byte: u8,
    log_idx: u64,
) -> Log {
    let ev = AssetFeeSet {
        assetId: asset_id,
        depositBps: deposit_bps,
        withdrawBps: withdraw_bps,
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

fn deposit_escrowed_log(id: u64, block_n: u64, block_ts: u64, tx_byte: u8, log_idx: u64) -> Log {
    let ev = DepositEscrowed {
        id: U256::from(id),
        payer: Address::repeat_byte(0x01),
        recipient: Address::repeat_byte(0x02),
        publicAssetId: ASSET_ID,
        publicIn: 100,
        feeBpsAtSubmit: 0,
        cm: B256::repeat_byte(0x03),
        cvDepX: U256::ZERO,
        cvDepY: U256::ZERO,
        rcv: U256::ZERO,
        clueRx: U256::ZERO,
        clueRy: U256::ZERO,
        ephPubX: U256::ZERO,
        ephPubY: U256::ZERO,
        ciphertext: vec![0u8; 2].into(),
        feeAssetId: FEE_ASSET_ID,
        feeIn: 3,
        feeCm: B256::repeat_byte(0x04),
        feeCvDepX: U256::ZERO,
        feeCvDepY: U256::ZERO,
        feeRcv: U256::ZERO,
        feeClueRx: U256::ZERO,
        feeClueRy: U256::ZERO,
        feeEphPubX: U256::ZERO,
        feeEphPubY: U256::ZERO,
        feeCiphertext: vec![0u8; 2].into(),
    };
    build_log(ev.encode_log_data(), block_n, block_ts, tx_byte, log_idx)
}

fn deposit_flushed_log(id: u64, block_n: u64, block_ts: u64, tx_byte: u8, log_idx: u64) -> Log {
    let ev = DepositFlushed {
        id: U256::from(id),
        cm: B256::repeat_byte(0x03),
    };
    build_log(ev.encode_log_data(), block_n, block_ts, tx_byte, log_idx)
}

/// No chains and no metadata RPC, so the decimals sweep is a no-op and these
/// tests exercise event consumption alone.
fn empty_ctx(pool: database::DbPool) -> ConsumeCtx {
    ConsumeCtx {
        pool,
        token_meta: Arc::new(std::collections::HashMap::new()),
        refresh: Arc::new(RefreshGate::new()),
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
    let _ = tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

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

/// Registration emits `AssetRegistered` and `AssetFeeSet` in one transaction.
/// The rates must land without disturbing `token` or `scale`, which the fee
/// upsert does not carry.
#[tokio::test]
async fn asset_fee_set_persists_rates_without_clobbering_the_registry() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    insert_log(
        &pool,
        CHAIN_A,
        &asset_registered_log(ASSET_ID, 0xde, 100, 1_700_000_000, 0x01, 0),
        EventKind::AssetRegistered,
    )
    .await;
    // Zero deposit, non-zero withdraw: a zero rate must persist as a rate, not
    // read back as "unset".
    insert_log(
        &pool,
        CHAIN_A,
        &asset_fee_set_log(ASSET_ID, 0, 20, 100, 1_700_000_000, 0x01, 1),
        EventKind::AssetFeeSet,
    )
    .await;

    let ctx = empty_ctx(pool.clone());
    let _ = tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    use database::schema::assets;
    let mut conn = pool.get().await.unwrap();
    let row: (Vec<u8>, BigDecimal, Option<i16>, Option<i16>) = assets::table
        .filter(assets::chain_id.eq(CHAIN_A))
        .select((
            assets::token,
            assets::scale,
            assets::deposit_bps,
            assets::withdraw_bps,
        ))
        .first(&mut conn)
        .await
        .unwrap();
    assert_eq!(row.0[0], 0xde, "token survived the fee upsert");
    assert_eq!(
        row.1,
        BigDecimal::from_str("1000000").unwrap(),
        "scale survived"
    );
    assert_eq!(row.2, Some(0), "a zero deposit rate is stored, not dropped");
    assert_eq!(row.3, Some(20));
}

/// A rate change long after registration replaces the stored pair rather than
/// only filling a gap, and still leaves the registry columns alone.
#[tokio::test]
async fn asset_fee_set_replaces_earlier_rates() {
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
        &asset_fee_set_log(ASSET_ID, 25, 25, 100, 1_700_000_000, 0x01, 1),
        EventKind::AssetFeeSet,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &asset_fee_set_log(ASSET_ID, 0, 50, 101, 1_700_000_100, 0x02, 0),
        EventKind::AssetFeeSet,
    )
    .await;

    let ctx = empty_ctx(pool.clone());
    let _ = tick_chain(&ctx, CHAIN_A, 101).await.unwrap();

    use database::schema::assets;
    let mut conn = pool.get().await.unwrap();
    let row: (Vec<u8>, Option<i16>, Option<i16>) = assets::table
        .filter(assets::chain_id.eq(CHAIN_A))
        .select((assets::token, assets::deposit_bps, assets::withdraw_bps))
        .first(&mut conn)
        .await
        .unwrap();
    assert_eq!(row.0[0], 0xde);
    assert_eq!(row.1, Some(0), "latest rates win");
    assert_eq!(row.2, Some(50));
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
    let _ = tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

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
    let _ = tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    {
        use database::schema::consumer_cursors;
        // Taken from `Owner`, not spelled out: this consumer commits under
        // `Owner::Protocol.cursor_name()`, and a literal that no longer matches
        // it would rewind nothing — leaving the second tick with an empty
        // window and the assertions below passing without a replay.
        let cursor_name = database::reorg::Owner::Protocol.cursor_name();
        let mut conn = pool.get().await.unwrap();
        diesel::update(consumer_cursors::table)
            .filter(consumer_cursors::name.eq(cursor_name))
            .filter(consumer_cursors::chain_id.eq(CHAIN_A))
            .set(consumer_cursors::last_event_id.eq(0))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let _ = tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

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

/// A flush records where in its transaction it sat, not only the transaction:
/// a `Bundler` transaction can hold a flush and a transfer under one hash, and
/// the explorer tells them apart by `flushed_log_index`.
#[tokio::test]
async fn deposit_flushed_records_its_log_index() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    insert_log(
        &pool,
        CHAIN_A,
        &deposit_escrowed_log(1, 100, 1_700_000_000, 0x20, 0),
        EventKind::DepositEscrowed,
    )
    .await;
    insert_log(
        &pool,
        CHAIN_A,
        &deposit_flushed_log(1, 101, 1_700_000_060, 0x21, 3),
        EventKind::DepositFlushed,
    )
    .await;

    let ctx = empty_ctx(pool.clone());
    let _ = tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    use database::schema::deposit_escrowed_events as d;
    let mut conn = pool.get().await.unwrap();
    let row: (Option<i64>, Option<Vec<u8>>, Option<i32>) = d::table
        .filter(d::chain_id.eq(CHAIN_A))
        .select((
            d::flushed_at_block,
            d::flushed_tx_hash,
            d::flushed_log_index,
        ))
        .first(&mut conn)
        .await
        .unwrap();
    assert_eq!(row, (Some(101), Some(vec![0x21; 32]), Some(3)));
}

/// The fee note's asset is digest preimage and independent of the deposit's, so
/// it lands in its own column exactly as logged.
#[tokio::test]
async fn deposit_escrowed_persists_the_fee_asset() {
    let (pool, _serial) = fresh_pool().await;
    insert_chain_state(&pool, CHAIN_A).await;

    insert_log(
        &pool,
        CHAIN_A,
        &deposit_escrowed_log(1, 100, 1_700_000_000, 0x20, 0),
        EventKind::DepositEscrowed,
    )
    .await;

    let ctx = empty_ctx(pool.clone());
    let _ = tick_chain(&ctx, CHAIN_A, 100).await.unwrap();

    use database::schema::deposit_escrowed_events as d;
    let mut conn = pool.get().await.unwrap();
    let row: (i64, i64, BigDecimal) = d::table
        .filter(d::chain_id.eq(CHAIN_A))
        .select((d::public_asset_id, d::fee_asset_id, d::fee_in))
        .first(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        row,
        (ASSET_ID as i64, FEE_ASSET_ID as i64, BigDecimal::from(3))
    );
}
