// snarkjs-shaped `tree_update_batch.circom` witness builder.

use crate::adapters::calldata::MAX_N_BATCH;
use crate::services::prover::TreeUpdateBatchWitness;
use crate::services::tree::{AdvancedState, ReservedSlot};
use alloy::primitives::{FixedBytes, U256};
use fmd_crypto::tree::Field;

/// Per-pair deposit binding inputs (flush path). One per active pair.
#[derive(Debug, Clone)]
pub struct PairDeposit {
    pub cv_dep0: [U256; 2],
    pub cv_dep1: [U256; 2],
    pub pair_asset: u64,
    pub pair_public_in: u64,
    pub rcv_total: U256,
}

/// Spend-side witness (N=1, is_deposit=0). cms[0..2] = output cms; rest
/// zero. Spend's transact_2x2 SNARK already proves conservation, so the
/// per-pair aggregate is skipped (is_deposit[0] = 0).
pub fn build_n1(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    cm0: &FixedBytes<32>,
    cm1: &FixedBytes<32>,
    cv_dep0: &[U256; 2],
    cv_dep1: &[U256; 2],
    z: String,
) -> TreeUpdateBatchWitness {
    let mut cms: Vec<String> = vec!["0".to_string(); 2 * MAX_N_BATCH];
    cms[0] = bytes32_to_dec(&cm0.0);
    cms[1] = bytes32_to_dec(&cm1.0);
    let mut cv_dep: Vec<[String; 2]> = vec![["0".to_string(), "0".to_string()]; 2 * MAX_N_BATCH];
    cv_dep[0] = [u256_to_dec(&cv_dep0[0]), u256_to_dec(&cv_dep0[1])];
    cv_dep[1] = [u256_to_dec(&cv_dep1[0]), u256_to_dec(&cv_dep1[1])];
    let pair_asset = vec!["0".to_string(); MAX_N_BATCH];
    let pair_public_in = vec!["0".to_string(); MAX_N_BATCH];
    let is_deposit = vec!["0".to_string(); MAX_N_BATCH];
    let rcv_total = vec!["0".to_string(); MAX_N_BATCH];
    build_inner(
        slot,
        advanced,
        1,
        cms,
        cv_dep,
        pair_asset,
        pair_public_in,
        is_deposit,
        rcv_total,
        z,
    )
}

/// Flush-side witness. `real_cms` length must equal `2 * actual_count`;
/// `pairs` length must equal `actual_count`. All padding slots emit zero
/// (cm, cv_dep, pair_asset, pair_public_in, is_deposit, rcv_total).
pub fn build_batch(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    real_cms: &[FixedBytes<32>],
    pairs: &[PairDeposit],
    actual_count: u64,
    z: String,
) -> TreeUpdateBatchWitness {
    debug_assert_eq!(real_cms.len(), (2 * actual_count) as usize);
    debug_assert_eq!(pairs.len(), actual_count as usize);

    let mut cms: Vec<String> = vec!["0".to_string(); 2 * MAX_N_BATCH];
    for (i, cm) in real_cms.iter().enumerate() {
        cms[i] = bytes32_to_dec(&cm.0);
    }

    let mut cv_dep: Vec<[String; 2]> = vec![["0".to_string(), "0".to_string()]; 2 * MAX_N_BATCH];
    let mut pair_asset: Vec<String> = vec!["0".to_string(); MAX_N_BATCH];
    let mut pair_public_in: Vec<String> = vec!["0".to_string(); MAX_N_BATCH];
    let mut is_deposit: Vec<String> = vec!["0".to_string(); MAX_N_BATCH];
    let mut rcv_total: Vec<String> = vec!["0".to_string(); MAX_N_BATCH];

    for (i, p) in pairs.iter().enumerate() {
        cv_dep[2 * i] = [u256_to_dec(&p.cv_dep0[0]), u256_to_dec(&p.cv_dep0[1])];
        cv_dep[2 * i + 1] = [u256_to_dec(&p.cv_dep1[0]), u256_to_dec(&p.cv_dep1[1])];
        pair_asset[i] = p.pair_asset.to_string();
        pair_public_in[i] = p.pair_public_in.to_string();
        is_deposit[i] = "1".to_string();
        rcv_total[i] = u256_to_dec(&p.rcv_total);
    }

    build_inner(
        slot,
        advanced,
        actual_count,
        cms,
        cv_dep,
        pair_asset,
        pair_public_in,
        is_deposit,
        rcv_total,
        z,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_inner(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    actual_count: u64,
    cms: Vec<String>,
    cv_dep: Vec<[String; 2]>,
    pair_asset: Vec<String>,
    pair_public_in: Vec<String>,
    is_deposit: Vec<String>,
    rcv_total: Vec<String>,
    z: String,
) -> TreeUpdateBatchWitness {
    TreeUpdateBatchWitness {
        z,
        old_root: field_to_dec(&slot.old_root),
        new_root: field_to_dec(&advanced.new_root),
        start_index: slot.start_index.to_string(),
        actual_count: actual_count.to_string(),
        cms,
        cv_dep,
        pair_asset,
        pair_public_in,
        is_deposit,
        rcv_total,
        frontier_in: slot
            .old_frontier
            .iter()
            .map(|row| {
                [
                    field_to_dec(&row[0]),
                    field_to_dec(&row[1]),
                    field_to_dec(&row[2]),
                ]
            })
            .collect(),
    }
}

fn field_to_dec(b: &Field) -> String {
    U256::from_be_bytes(*b).to_string()
}

fn bytes32_to_dec(b: &[u8; 32]) -> String {
    U256::from_be_bytes(*b).to_string()
}

fn u256_to_dec(v: &U256) -> String {
    v.to_string()
}
