//! Shared building blocks for single-transact pipelines (spend + swap).
//!
//! Both pipelines insert `TRANSACT_OUT` leaves, run the same
//! `tree_update_batch` SNARK over them, and differ only in: (a) which
//! contract they target, (b) how they encode the calldata, and (c) which
//! payload-shape checks they apply. This module owns everything they share.

use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{MAX_L_BATCH, build_tu_batch_pub_inputs};
use crate::adapters::parse::{parse_address, parse_b32, parse_u256};
use crate::domain::dto::{PubInputsDto, TRANSACT_OUT};
use crate::domain::error::{AppError, AppResult};
use crate::domain::fiat_shamir;
use crate::services::prover::{TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::tree::{AdvancedState, ReservedSlot};
use crate::services::witness;
use alloy::primitives::{Address, FixedBytes, U256};
use std::sync::Arc;

/// A spend inserts one leaf per transact output.
pub const SPEND_LEAVES: usize = TRANSACT_OUT;

/// What a payload's transact proof must be bound to for this relayer to be
/// able to land it. Named fields rather than positional arguments: both are
/// checked against wallet-supplied values, and `chain_id`/`relayer` are easy
/// to transpose silently.
#[derive(Debug, Clone, Copy)]
pub struct TransactBinding {
    pub chain_id: i64,
    /// Address the proof must name as `relayer`. Usually this relayer's
    /// signer; for a native unshield it is the `NativeAdapter`, which drives
    /// `MASP.withdraw` itself and so is the pool's caller.
    pub relayer: Address,
}

impl TransactBinding {
    /// Reject payloads that cannot possibly land, before the prover runs.
    /// Each of these is an on-chain revert that would otherwise cost a
    /// multi-second Groth16 first.
    pub fn check(&self, pi: &PubInputsDto) -> AppResult<()> {
        if pi.chain_id != self.chain_id as u64 {
            return Err(AppError::BadRequest(format!(
                "pubInputs.chainId ({}) must equal the request chainId ({})",
                pi.chain_id, self.chain_id
            )));
        }
        let bound = parse_address(&pi.relayer)?;
        if bound != self.relayer {
            return Err(AppError::BadRequest(format!(
                "pubInputs.relayer ({bound}) must equal the expected relayer ({})",
                self.relayer
            )));
        }
        // Pairwise over the whole input shape: the circuit constrains every
        // pair, so any repeat is a double-spend the pool would reject.
        for i in 0..pi.nullifier.len() {
            for j in (i + 1)..pi.nullifier.len() {
                if pi.nullifier[i] == pi.nullifier[j] {
                    return Err(AppError::BadRequest(format!(
                        "nullifiers {i} and {j} are equal; all must differ"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Parsed leg-1 output commitments + value commitments, ready to feed the
/// tree mirror, the Fiat-Shamir transcript, and the SNARK witness builder.
#[derive(Clone, Copy)]
pub struct SpendInputs {
    pub cms: [FixedBytes<32>; SPEND_LEAVES],
    pub cv_deps: [[U256; 2]; SPEND_LEAVES],
}

impl SpendInputs {
    /// `(cm, cv_dep)` leaves in insertion order, as `TreeMirror` wants them.
    pub fn leaves(&self) -> Vec<(fmd_crypto::tree::Field, [U256; 2])> {
        self.cms
            .iter()
            .zip(self.cv_deps.iter())
            .map(|(cm, cv)| (cm.0, *cv))
            .collect()
    }
}

/// Padded arrays expected by `build_tu_batch_pub_inputs` and `compute_z`.
/// Leaf slots `0..SPEND_LEAVES` carry the spend; everything else is zero.
pub type PaddedBatchArrays = (
    [FixedBytes<32>; MAX_L_BATCH],
    [[U256; 2]; MAX_L_BATCH],
    [u64; MAX_L_BATCH],
    [u64; MAX_L_BATCH],
    [u8; MAX_L_BATCH],
);

pub fn parse_spend_inputs(pi: &PubInputsDto) -> AppResult<SpendInputs> {
    let mut cms = [FixedBytes::<32>::ZERO; SPEND_LEAVES];
    let mut cv_deps = [[U256::ZERO; 2]; SPEND_LEAVES];
    for i in 0..SPEND_LEAVES {
        cms[i] = parse_b32(&pi.out_cm[i])?;
        cv_deps[i] = [
            parse_u256(&pi.out_cv_dep[i].x)?,
            parse_u256(&pi.out_cv_dep[i].y)?,
        ];
    }
    Ok(SpendInputs { cms, cv_deps })
}

/// Build the padded arrays a spend feeds to the SNARK + calldata.
/// `is_deposit` stays all-zero: the transact SNARK already proves
/// conservation, so the per-leaf deposit binding is intentionally skipped.
pub fn build_padded_spend_arrays(inputs: &SpendInputs) -> PaddedBatchArrays {
    let mut cms_padded = [FixedBytes::<32>::ZERO; MAX_L_BATCH];
    let mut cv_deps_padded = [[U256::ZERO, U256::ZERO]; MAX_L_BATCH];
    cms_padded[..SPEND_LEAVES].copy_from_slice(&inputs.cms);
    cv_deps_padded[..SPEND_LEAVES].copy_from_slice(&inputs.cv_deps);
    (
        cms_padded,
        cv_deps_padded,
        [0u64; MAX_L_BATCH],
        [0u64; MAX_L_BATCH],
        [0u8; MAX_L_BATCH],
    )
}

/// Run the tree-update SNARK for one spend: derive `z`, build the witness,
/// hand off to the prover. Caller already holds the mirror lock.
pub async fn prove_spend(
    prover: &Arc<dyn TreeUpdateBatchProver>,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    inputs: &SpendInputs,
) -> AppResult<TreeUpdateBatchProof> {
    let (cms_padded, cv_deps_padded, la, lpi, is_dep) = build_padded_spend_arrays(inputs);
    let z = fiat_shamir::compute_z(
        &slot.old_root,
        &advanced.new_root,
        slot.start_index,
        SPEND_LEAVES as u64,
        &cms_padded,
        &cv_deps_padded,
        &la,
        &lpi,
        &is_dep,
    );
    let w = witness::build_spend(slot, advanced, &inputs.cms, &inputs.cv_deps, z);
    prover.prove(w).await
}

/// Encode the `TreeUpdateBatch` public-inputs struct that both spend and
/// swap calldata builders embed identically (`actualCount = SPEND_LEAVES`,
/// rest zero-padded).
pub fn build_tu_pi_for_spend(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    arrays: PaddedBatchArrays,
) -> IMasp::TreeUpdateBatch {
    let (cms_padded, cv_deps_padded, la, lpi, is_dep) = arrays;
    build_tu_batch_pub_inputs(
        slot.start_index,
        &slot.old_root,
        &advanced.new_root,
        cms_padded,
        cv_deps_padded,
        la,
        lpi,
        is_dep,
        SPEND_LEAVES as u64,
    )
}
