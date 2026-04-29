use crate::error::ExplorerIndexerError;
use crate::repositories::{
    asset_flows::{self, NewAssetFlow},
    assets::{self, UpsertAsset},
    intent_events::{self, NewIntentEscrowed},
    raw_events::RawEventRow,
    tree_advances::{self, NewTreeAdvance},
};
use crate::util::u256_to_bigdecimal;
use alloy::primitives::{Address, B256, U256};
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

pub async fn asset_moved(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    asset_id: u64,
    token: Address,
    in_amount: U256,
    out_amount: U256,
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
        },
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn intent_escrowed(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    id: U256,
    payer: Address,
    recipient: Address,
    public_asset_id: u64,
    public_in: u64,
    fee_bps_at_submit: u16,
    cm0: B256,
    cm1: B256,
    cv_dep0_x: U256,
    cv_dep0_y: U256,
    cv_dep1_x: U256,
    cv_dep1_y: U256,
    rcv_total: U256,
    aux: serde_json::Value,
) -> Result<(), ExplorerIndexerError> {
    intent_events::insert(
        pool,
        NewIntentEscrowed {
            chain_id,
            block_number: row.block_number,
            log_index: row.log_index,
            intent_id: u256_to_bigdecimal(id),
            payer: payer.as_slice().to_vec(),
            recipient: recipient.as_slice().to_vec(),
            public_asset_id: public_asset_id as i64,
            public_in: u256_to_bigdecimal(U256::from(public_in)),
            fee_bps_at_submit: i32::from(fee_bps_at_submit),
            cm0: cm0.0.to_vec(),
            cm1: cm1.0.to_vec(),
            cv_dep0_x: u256_to_bigdecimal(cv_dep0_x),
            cv_dep0_y: u256_to_bigdecimal(cv_dep0_y),
            cv_dep1_x: u256_to_bigdecimal(cv_dep1_x),
            cv_dep1_y: u256_to_bigdecimal(cv_dep1_y),
            rcv_total: u256_to_bigdecimal(rcv_total),
            aux,
            submitted_at_block: row.block_number,
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
        },
    )
    .await?;
    Ok(())
}

pub async fn intent_flushed(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    id: U256,
) -> Result<(), ExplorerIndexerError> {
    intent_events::mark_flushed(pool, chain_id, u256_to_bigdecimal(id), row.block_number).await?;
    Ok(())
}

pub async fn intent_canceled(
    pool: &DbPool,
    chain_id: i64,
    row: &RawEventRow,
    id: U256,
) -> Result<(), ExplorerIndexerError> {
    intent_events::mark_canceled(pool, chain_id, u256_to_bigdecimal(id), row.block_number).await?;
    Ok(())
}

/// Encode the per-output aux blob as JSON for the `aux` column.
#[allow(clippy::too_many_arguments)]
pub fn encode_aux(
    clue_rx0: U256,
    clue_ry0: U256,
    eph_pub_x0: U256,
    eph_pub_y0: U256,
    ciphertext0: &[u8],
    clue_rx1: U256,
    clue_ry1: U256,
    eph_pub_x1: U256,
    eph_pub_y1: U256,
    ciphertext1: &[u8],
) -> serde_json::Value {
    json!([
        {
            "clueRx": clue_rx0.to_string(),
            "clueRy": clue_ry0.to_string(),
            "ephPubX": eph_pub_x0.to_string(),
            "ephPubY": eph_pub_y0.to_string(),
            "ciphertext": format!("0x{}", hex::encode(ciphertext0)),
        },
        {
            "clueRx": clue_rx1.to_string(),
            "clueRy": clue_ry1.to_string(),
            "ephPubX": eph_pub_x1.to_string(),
            "ephPubY": eph_pub_y1.to_string(),
            "ciphertext": format!("0x{}", hex::encode(ciphertext1)),
        }
    ])
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
        NewTreeAdvance {
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
