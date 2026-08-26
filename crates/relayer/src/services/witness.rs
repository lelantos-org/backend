//! snarkjs-shaped witness builder for `tree_update_batch.circom`.

use crate::adapters::calldata::MAX_L_BATCH;
use crate::services::prover::TreeUpdateBatchWitness;
use crate::services::tree::{AdvancedState, ReservedSlot};
use alloy::primitives::{FixedBytes, U256};
use fmd_crypto::tree::Field;

/// One escrowed deposit leaf. The circuit pins
/// `cv_dep == public_in · V^asset + rcv · H` for every slot flagged
/// `is_deposit`, so each leaf carries its own binding and there is no per-pair
/// aggregate.
#[derive(Debug, Clone)]
pub struct LeafDeposit {
    pub cv_dep: [U256; 2],
    pub leaf_asset: u64,
    pub leaf_public_in: u64,
    pub rcv: U256,
}

/// Spend-side witness: `TRANSACT_OUT` leaves, all with `is_deposit = 0`. The
/// transact SNARK already proves conservation, so the per-leaf deposit binding is
/// skipped and `rcv` stays zero.
pub fn build_spend(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    cms_real: &[FixedBytes<32>],
    cv_deps_real: &[[U256; 2]],
    z: String,
) -> TreeUpdateBatchWitness {
    debug_assert_eq!(cms_real.len(), cv_deps_real.len());

    let mut cms: Vec<String> = vec!["0".to_string(); MAX_L_BATCH];
    for (i, cm) in cms_real.iter().enumerate() {
        cms[i] = bytes32_to_dec(&cm.0);
    }
    let mut cv_dep: Vec<[String; 2]> = vec![["0".to_string(), "0".to_string()]; MAX_L_BATCH];
    for (i, cv) in cv_deps_real.iter().enumerate() {
        cv_dep[i] = [u256_to_dec(&cv[0]), u256_to_dec(&cv[1])];
    }

    build_inner(
        slot,
        advanced,
        cms_real.len() as u64,
        cms,
        cv_dep,
        vec!["0".to_string(); MAX_L_BATCH],
        vec!["0".to_string(); MAX_L_BATCH],
        vec!["0".to_string(); MAX_L_BATCH],
        vec!["0".to_string(); MAX_L_BATCH],
        z,
    )
}

/// Flush-side witness, one leaf per escrowed deposit, so `deposits.len()` is the
/// leaf count. Padding slots emit zero for `cm`, `cv_dep`, `leaf_asset`,
/// `leaf_public_in`, `is_deposit` and `rcv`.
pub fn build_batch(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    real_cms: &[FixedBytes<32>],
    deposits: &[LeafDeposit],
    z: String,
) -> TreeUpdateBatchWitness {
    debug_assert_eq!(real_cms.len(), deposits.len());

    let mut cms: Vec<String> = vec!["0".to_string(); MAX_L_BATCH];
    for (i, cm) in real_cms.iter().enumerate() {
        cms[i] = bytes32_to_dec(&cm.0);
    }

    let mut cv_dep: Vec<[String; 2]> = vec![["0".to_string(), "0".to_string()]; MAX_L_BATCH];
    let mut leaf_asset: Vec<String> = vec!["0".to_string(); MAX_L_BATCH];
    let mut leaf_public_in: Vec<String> = vec!["0".to_string(); MAX_L_BATCH];
    let mut is_deposit: Vec<String> = vec!["0".to_string(); MAX_L_BATCH];
    let mut rcv: Vec<String> = vec!["0".to_string(); MAX_L_BATCH];

    for (i, d) in deposits.iter().enumerate() {
        cv_dep[i] = [u256_to_dec(&d.cv_dep[0]), u256_to_dec(&d.cv_dep[1])];
        leaf_asset[i] = d.leaf_asset.to_string();
        leaf_public_in[i] = d.leaf_public_in.to_string();
        is_deposit[i] = "1".to_string();
        rcv[i] = u256_to_dec(&d.rcv);
    }

    build_inner(
        slot,
        advanced,
        deposits.len() as u64,
        cms,
        cv_dep,
        leaf_asset,
        leaf_public_in,
        is_deposit,
        rcv,
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
    leaf_asset: Vec<String>,
    leaf_public_in: Vec<String>,
    is_deposit: Vec<String>,
    rcv: Vec<String>,
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
        leaf_asset,
        leaf_public_in,
        is_deposit,
        rcv,
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
