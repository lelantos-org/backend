// Fiat-Shamir challenge derivation for `tree_update_batch.circom`.
// Mirrors `contracts/src/lib/PubInputs.sol :: compress(TreeUpdateBatch)`
// so the relayer feeds the same `z` to snarkjs that the contract derives
// from calldata. Coefficient layout (4 + 9*MAX_N_BATCH = 76 entries):
//   [0]                                  oldRoot
//   [1]                                  newRoot
//   [2]                                  startIndex
//   [3]                                  actualCount
//   [4 .. 3 + 2*MAX_N]                   cms[0 .. 2*MAX_N-1]
//   [4 + 2*MAX_N .. 3 + 6*MAX_N]         cvDeps flat (x0,y0,x1,y1,...)
//   [4 + 6*MAX_N .. 3 + 7*MAX_N]         pairAsset[0..MAX_N-1]
//   [4 + 7*MAX_N .. 3 + 8*MAX_N]         pairPublicIn[0..MAX_N-1]
//   [4 + 8*MAX_N .. 3 + 9*MAX_N]         isDeposit[0..MAX_N-1]

use crate::adapters::calldata::MAX_N_BATCH;
use alloy::primitives::{FixedBytes, U256, keccak256};
use alloy::sol_types::SolValue;
use fmd_crypto::tree::Field;
use std::sync::LazyLock;

/// BN254 scalar field order.
static BN254_R: LazyLock<U256> = LazyLock::new(|| {
    U256::from_str_radix(
        "21888242871839275222246405745257275088548364400416034343698204186575808495617",
        10,
    )
    .expect("BN254 modulus literal")
});

#[allow(clippy::too_many_arguments)]
pub fn compute_z(
    old_root: &Field,
    new_root: &Field,
    start_index: u64,
    actual_count: u64,
    cms: &[FixedBytes<32>; 2 * MAX_N_BATCH],
    cv_deps: &[[U256; 2]; 2 * MAX_N_BATCH],
    pair_asset: &[u64; MAX_N_BATCH],
    pair_public_in: &[u64; MAX_N_BATCH],
    is_deposit: &[u8; MAX_N_BATCH],
) -> String {
    let mut coeffs: Vec<U256> = Vec::with_capacity(4 + 9 * MAX_N_BATCH);
    coeffs.push(U256::from_be_bytes(*old_root));
    coeffs.push(U256::from_be_bytes(*new_root));
    coeffs.push(U256::from(start_index));
    coeffs.push(U256::from(actual_count));
    for cm in cms.iter() {
        coeffs.push(U256::from_be_bytes(cm.0));
    }
    for pt in cv_deps.iter() {
        coeffs.push(pt[0]);
        coeffs.push(pt[1]);
    }
    for v in pair_asset.iter() {
        coeffs.push(U256::from(*v));
    }
    for v in pair_public_in.iter() {
        coeffs.push(U256::from(*v));
    }
    for v in is_deposit.iter() {
        coeffs.push(U256::from(*v));
    }
    let encoded = coeffs.abi_encode();
    let h = keccak256(&encoded);
    let z = U256::from_be_bytes(h.0) % *BN254_R;
    z.to_string()
}
