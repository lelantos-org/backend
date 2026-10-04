//! Fiat-Shamir compression for `tree_update_batch.circom`.
//!
//! Mirrors `contracts/src/libs/PubInputs.sol :: compress(TreeUpdateBatch)`, and
//! `compressSpend` for the batch a spend implies, so the relayer feeds the
//! prover the `z` the contract derives from calldata and puts in calldata the
//! `digest` the circuit outputs. Every array is leaf-indexed. Coefficient
//! layout, `4 + 4 * MAX_L_BATCH` words:
//!
//! ```text
//! [0]                            oldRoot
//! [1]                            newRoot
//! [2]                            startIndex
//! [3]                            actualCount
//! [4 .. 3 + MAX_L]               cms[0 .. MAX_L-1]
//! [4 + MAX_L .. 3 + 2*MAX_L]     leafAsset[0 .. MAX_L-1]
//! [4 + 2*MAX_L .. 3 + 3*MAX_L]   leafPublicIn[0 .. MAX_L-1]
//! [4 + 3*MAX_L .. 3 + 4*MAX_L]   isDeposit[0 .. MAX_L-1]
//! ```
//!
//! `digest` is the circuit's `CoeffDigest` of those words
//! (`crypto::poseidon::coeff_digest`), and
//! `z = keccak256(abi.encode(coefficients ++ [digest])) mod r`.

use crate::domain::batch::{MAX_L_BATCH, PaddedBatch};
use crate::domain::error::{AppError, AppResult};
use crate::domain::field::BN254_R;
use alloy::primitives::{U256, keccak256};
use alloy::sol_types::SolValue;
use crypto::poseidon;
use crypto::tree::Field;

/// Words the batch polynomial is evaluated over, `PubInputs.BATCH_COEFFS`.
pub const BATCH_COEFFS: usize = 4 + 4 * MAX_L_BATCH;

/// What the relayer derives from a batch's coefficients: the digest word that
/// goes in calldata (`TreeUpdateBatch.digest`, `SpendTree.digest`) and the
/// challenge the prover takes as `z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchChallenge {
    pub digest: U256,
    pub z: U256,
}

impl BatchChallenge {
    /// The digest and the challenge a batch proof publishes: its decimal public
    /// signals are `[y, digest, z]`. `None` for any other shape.
    pub fn from_public_signals(signals: &[String]) -> Option<Self> {
        let [_y, digest, z] = signals else {
            return None;
        };
        Some(Self {
            digest: U256::from_str_radix(digest, 10).ok()?,
            z: U256::from_str_radix(z, 10).ok()?,
        })
    }
}

/// The digest and the challenge of `batch` advancing the tree from `old_root`
/// to `new_root` at `start_index`.
///
/// Fails on a coefficient at or above the field modulus, which has no digest.
pub fn compress(
    old_root: &Field,
    new_root: &Field,
    start_index: u64,
    batch: &PaddedBatch,
) -> AppResult<BatchChallenge> {
    let mut words = coefficients(old_root, new_root, start_index, batch);
    let bytes: Vec<Field> = words.iter().map(U256::to_be_bytes).collect();
    let digest = poseidon::coeff_digest(&bytes)
        .map(U256::from_be_bytes)
        .map_err(|e| AppError::Internal(format!("coefficient digest: {e}")))?;
    words.push(digest);
    let z = U256::from_be_bytes(keccak256(words.abi_encode()).0) % *BN254_R;
    Ok(BatchChallenge { digest, z })
}

/// The coefficient vector, in the order the module doc lays out.
fn coefficients(
    old_root: &Field,
    new_root: &Field,
    start_index: u64,
    batch: &PaddedBatch,
) -> Vec<U256> {
    // One more than the coefficients: `compress` appends the digest word.
    let mut coeffs: Vec<U256> = Vec::with_capacity(BATCH_COEFFS + 1);
    coeffs.push(U256::from_be_bytes(*old_root));
    coeffs.push(U256::from_be_bytes(*new_root));
    coeffs.push(U256::from(start_index));
    coeffs.push(U256::from(batch.actual_count));
    coeffs.extend(batch.cms.iter().map(|cm| U256::from_be_bytes(cm.0)));
    coeffs.extend(batch.leaf_asset.iter().copied().map(U256::from));
    coeffs.extend(batch.leaf_public_in.iter().copied().map(U256::from));
    coeffs.extend(batch.is_deposit.iter().copied().map(U256::from));
    debug_assert_eq!(coeffs.len(), BATCH_COEFFS);
    coeffs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::batch_vectors;

    /// `compress` mirrors `PubInputs.compress(TreeUpdateBatch)`, and the circuit
    /// commits to the same coefficients and Horner-evaluates them at the same
    /// `z`. All three must agree, so pinning the published vectors catches a
    /// layout drift that would otherwise surface only on-chain: as
    /// `TreeUpdateRejected` on the flush path, and as `ProofRejected` on a spend,
    /// whose two proofs the batched verifier checks in one pairing and cannot
    /// attribute.
    #[test]
    fn every_published_vector_matches_its_digest_and_z() {
        for v in batch_vectors::load() {
            let got = compress(&v.old_root, &v.new_root, v.start_index, &v.batch)
                .unwrap_or_else(|e| panic!("{}: {e}", v.name));
            assert_eq!(got.digest, v.digest, "{} digest", v.name);
            assert_eq!(got.z, v.z, "{} z", v.name);
        }
    }

    /// A coefficient at or above the modulus has no digest: the contract reverts
    /// `CoefficientOutOfField` on it, so nothing is derived from it here either.
    #[test]
    fn a_non_canonical_coefficient_has_no_challenge() {
        let batch = PaddedBatch::from_spend(&[BN254_R.to_be_bytes().into()]);
        assert!(compress(&[0u8; 32], &[0u8; 32], 0, &batch).is_err());
    }

    /// A proof's signals are read by position, and anything but `[y, digest, z]`
    /// is not a batch proof's.
    #[test]
    fn a_challenge_is_read_from_the_signals_after_y() {
        let signals = ["7", "11", "13"].map(String::from);
        assert_eq!(
            BatchChallenge::from_public_signals(&signals),
            Some(BatchChallenge {
                digest: U256::from(11),
                z: U256::from(13),
            })
        );
        assert_eq!(BatchChallenge::from_public_signals(&signals[..2]), None);
    }
}
