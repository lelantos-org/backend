//! The in-circuit Merkle leaf.

use crate::domain::error::{AppError, AppResult};
use alloy::primitives::U256;
use crypto::poseidon as common_poseidon;
use crypto::tree::Field;

/// Domain-separation tag for Merkle leaf hashing, mirroring `TAG_LEAF` in
/// `circuits/src/lib/tags.circom`. `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x,
/// cv_dep_y)`, so a spender can rebuild the same leaf hash from `(cm, cv_dep)`
/// without learning anything else about the deposit.
const TAG_LEAF: u64 = 10;

/// Compute the in-circuit Merkle leaf:
/// `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
///
/// Must match `tree_update_batch.circom` byte for byte; drift here desyncs the
/// relayer's mirror from the on-chain tree.
pub(super) fn leaf_hash(cm: &Field, cv_dep: &[U256; 2]) -> AppResult<Field> {
    let mut tag = [0u8; 32];
    tag[31] = TAG_LEAF as u8;
    let cv_x = cv_dep[0].to_be_bytes::<32>();
    let cv_y = cv_dep[1].to_be_bytes::<32>();
    common_poseidon::hash_bytes_be(&[&tag, cm, &cv_x, &cv_y])
        .map_err(|e| AppError::Internal(format!("leaf_hash: {}", e)))
}
