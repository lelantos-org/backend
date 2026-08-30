use crate::error::ExplorerIndexerError;
use crate::repositories::{
    asset_flows::{self, NewAssetFlow},
    assets::{self, UpsertAsset, UpsertAssetFee},
    deposit_events::{self, NewDepositEscrowed},
    raw_events::RawEventRow,
    tree_advances::{self, TreeAdvanceRow},
};
use crate::util::u256_to_bigdecimal;
use alloy::primitives::{Address, B256, U256};
use bigdecimal::BigDecimal;
use chain_types::decode::DepositFeeNote;
use database::DbPool;
use serde_json::json;

pub async fn asset_registered(
    pool: &DbPool,
    chain_id: i64,
    asset_id: u64,
    token: Address,
    scale: U256,
) -> Result<(), ExplorerIndexerError> {
    assets::upsert(
        pool,
        UpsertAsset {
            chain_id,
            asset_id_u64: asset_id as i64,
            token: token.as_slice().to_vec(),
            scale: u256_to_bigdecimal(scale),
        },
    )
    .await
}

/// Rates are mutable, so this replaces whatever was stored rather than filling
/// a gap: a later `AssetFeeSet` for the same asset is a rate change, not a
/// duplicate.
pub async fn asset_fee_set(
    pool: &DbPool,
    chain_id: i64,
    asset_id: u64,
    deposit_bps: u16,
    withdraw_bps: u16,
) -> Result<(), ExplorerIndexerError> {
    assets::upsert_fee(
        pool,
        UpsertAssetFee {
            chain_id,
            asset_id_u64: asset_id as i64,
            // Both are `uint16` on chain but bounded by `MAX_FEE_BPS` (2000),
            // so the cast to Postgres `SMALLINT` cannot lose a valid value.
            deposit_bps: deposit_bps as i16,
            withdraw_bps: withdraw_bps as i16,
        },
    )
    .await
}

// One argument per `DecodedEvent::AssetMoved` field, as with `deposit_escrowed`
// below; grouping them would only restate the variant.
#[allow(clippy::too_many_arguments)]
pub async fn asset_moved(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    asset_id: u64,
    token: Address,
    in_amount: U256,
    out_amount: U256,
    public_in: u64,
    public_out: u64,
) -> Result<(), ExplorerIndexerError> {
    asset_flows::insert(
        pool,
        NewAssetFlow {
            chain_id,
            block_number: row.block_number,
            log_index: row.log_index,
            asset_id_u64: asset_id as i64,
            token: token.as_slice().to_vec(),
            in_amount: u256_to_bigdecimal(in_amount),
            out_amount: u256_to_bigdecimal(out_amount),
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
            public_in: Some(BigDecimal::from(public_in)),
            public_out: Some(BigDecimal::from(public_out)),
        },
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn deposit_escrowed(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    id: U256,
    payer: Address,
    recipient: Address,
    public_asset_id: u64,
    public_in: u64,
    fee_bps_at_submit: u16,
    cm: B256,
    cv_dep_x: U256,
    cv_dep_y: U256,
    rcv: U256,
    aux: serde_json::Value,
    fee: DepositFeeNote,
) -> Result<(), ExplorerIndexerError> {
    deposit_events::insert(
        pool,
        NewDepositEscrowed {
            chain_id,
            block_number: row.block_number,
            log_index: row.log_index,
            deposit_id: u256_to_bigdecimal(id),
            payer: payer.as_slice().to_vec(),
            recipient: recipient.as_slice().to_vec(),
            public_asset_id: public_asset_id as i64,
            public_in: u256_to_bigdecimal(U256::from(public_in)),
            fee_bps_at_submit: i32::from(fee_bps_at_submit),
            cm: cm.0.to_vec(),
            cv_dep_x: u256_to_bigdecimal(cv_dep_x),
            cv_dep_y: u256_to_bigdecimal(cv_dep_y),
            rcv: u256_to_bigdecimal(rcv),
            aux,
            fee_in: u256_to_bigdecimal(U256::from(fee.fee_in)),
            fee_cm: fee.cm.0.to_vec(),
            fee_cv_dep_x: u256_to_bigdecimal(fee.cv_dep_x),
            fee_cv_dep_y: u256_to_bigdecimal(fee.cv_dep_y),
            fee_rcv: u256_to_bigdecimal(fee.rcv),
            // Built here rather than by the caller so the fee leaf's payload
            // keeps the same shape as the depositor's.
            fee_aux: encode_aux(
                fee.clue_rx,
                fee.clue_ry,
                fee.eph_pub_x,
                fee.eph_pub_y,
                &fee.ciphertext,
            ),
            // The digest the contract stored hashes `uint32(block.number)`,
            // which on Arbitrum is the L1 height rather than `row.block_number`.
            // Rows ingested before `evm_block_number` existed fall back to
            // `block_number`: correct on every chain except Arbitrum, whose rows
            // need an explicit repair.
            submitted_at_block: row.evm_block_number.unwrap_or(row.block_number),
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
        },
    )
    .await?;
    Ok(())
}

pub async fn deposit_flushed(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    id: U256,
) -> Result<(), ExplorerIndexerError> {
    deposit_events::mark_flushed(
        pool,
        chain_id,
        u256_to_bigdecimal(id),
        row.block_number,
        row.block_ts,
        row.tx_hash.clone(),
    )
    .await?;
    Ok(())
}

pub async fn deposit_canceled(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    id: U256,
) -> Result<(), ExplorerIndexerError> {
    deposit_events::mark_canceled(pool, chain_id, u256_to_bigdecimal(id), row.block_number).await?;
    Ok(())
}

/// Encode the deposit leaf's aux blob as JSON for the `aux` column. A deposit
/// occupies one leaf, so this is a single object rather than an array.
pub fn encode_aux(
    clue_rx: U256,
    clue_ry: U256,
    eph_pub_x: U256,
    eph_pub_y: U256,
    ciphertext: &[u8],
) -> serde_json::Value {
    json!({
        "clueRx": clue_rx.to_string(),
        "clueRy": clue_ry.to_string(),
        "ephPubX": eph_pub_x.to_string(),
        "ephPubY": eph_pub_y.to_string(),
        "ciphertext": format!("0x{}", hex::encode(ciphertext)),
    })
}

pub async fn root_advanced(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    start_index: u64,
    inserted: u64,
    old_root: B256,
    new_root: B256,
) -> Result<(), ExplorerIndexerError> {
    tree_advances::insert(
        pool,
        TreeAdvanceRow {
            chain_id,
            block_number: row.block_number,
            log_index: row.log_index,
            start_index: start_index as i64,
            inserted: inserted as i32,
            old_root: old_root.0.to_vec(),
            new_root: new_root.0.to_vec(),
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
        },
    )
    .await?;
    Ok(())
}
