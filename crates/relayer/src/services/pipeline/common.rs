//! Shared building blocks for single-pair transact pipelines (spend + swap).
//!
//! Both pipelines insert exactly two leaves, run the same `tree_update_batch`
//! SNARK with `actual_count = 1`, and differ only in: (a) which contract
//! they target, (b) how they encode the calldata, and (c) which payload-shape
//! checks they apply. This module owns everything they share.

use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{MAX_N_BATCH, build_tu_batch_pub_inputs};
use crate::adapters::parse::{parse_address, parse_b32, parse_u256};
use crate::domain::dto::PubInputsDto;
use crate::domain::error::{AppError, AppResult};
use crate::domain::fiat_shamir;
use crate::services::prover::{TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::tree::{AdvancedState, ReservedSlot};
use crate::services::witness;
use alloy::primitives::{Address, FixedBytes, U256};
use std::sync::Arc;

/// Single-pair transact ops always insert exactly two leaves.
pub const PAIR_LEAVES: usize = 2;

/// What a payload's `transact_2x2` proof must be bound to for this relayer to
/// be able to land it. Named fields rather than positional arguments: both are
/// checked against wallet-supplied values, and `chain_id`/`relayer` are easy to
/// transpose silently.
#[derive(Debug, Clone, Copy)]
pub struct TransactBinding {
    pub chain_id: i64,
    /// This relayer's signer. The proof pins an address, and only submissions
    /// from that address satisfy it.
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
                "pubInputs.relayer ({bound}) must equal this relayer's signer ({})",
                self.relayer
            )));
        }
        if pi.nullifier[0] == pi.nullifier[1] {
            return Err(AppError::BadRequest(
                "the two nullifiers must differ".into(),
            ));
        }
        Ok(())
    }
}

/// Parsed leg-1 commitment + value-commitment pair, ready to feed the tree
/// mirror, the Fiat-Shamir transcript, and the SNARK witness builder.
#[derive(Clone, Copy)]
pub struct PairInputs {
    pub cm0: FixedBytes<32>,
    pub cm1: FixedBytes<32>,
    pub cv_dep0: [U256; 2],
    pub cv_dep1: [U256; 2],
}

/// Padded arrays expected by `build_tu_batch_pub_inputs` and `compute_z`.
/// Pair slot 0 carries the active leg-1 pair; everything else is zero.
pub type PaddedBatchArrays = (
    [FixedBytes<32>; 2 * MAX_N_BATCH],
    [[U256; 2]; 2 * MAX_N_BATCH],
    [u64; MAX_N_BATCH],
    [u64; MAX_N_BATCH],
    [u8; MAX_N_BATCH],
);

pub fn parse_pair_inputs(pi: &PubInputsDto) -> AppResult<PairInputs> {
    Ok(PairInputs {
        cm0: parse_b32(&pi.out_cm[0])?,
        cm1: parse_b32(&pi.out_cm[1])?,
        cv_dep0: [
            parse_u256(&pi.out_cv_dep[0].x)?,
            parse_u256(&pi.out_cv_dep[0].y)?,
        ],
        cv_dep1: [
            parse_u256(&pi.out_cv_dep[1].x)?,
            parse_u256(&pi.out_cv_dep[1].y)?,
        ],
    })
}

/// Build the padded arrays a single-pair pipeline feeds to the SNARK +
/// calldata. `is_deposit[0]` is left at 0: leg-1 SNARK already proves
/// conservation; the per-pair aggregate is intentionally skipped.
pub fn build_padded_pair_arrays(inputs: &PairInputs) -> PaddedBatchArrays {
    let mut cms_padded = [FixedBytes::<32>::ZERO; 2 * MAX_N_BATCH];
    cms_padded[0] = inputs.cm0;
    cms_padded[1] = inputs.cm1;
    let mut cv_deps_padded = [[U256::ZERO, U256::ZERO]; 2 * MAX_N_BATCH];
    cv_deps_padded[0] = inputs.cv_dep0;
    cv_deps_padded[1] = inputs.cv_dep1;
    (
        cms_padded,
        cv_deps_padded,
        [0u64; MAX_N_BATCH],
        [0u64; MAX_N_BATCH],
        [0u8; MAX_N_BATCH],
    )
}

/// Run the tree-update SNARK for a single pair: derive `z`, build the
/// witness, hand off to the prover. Caller already holds the mirror lock.
pub async fn prove_pair(
    prover: &Arc<dyn TreeUpdateBatchProver>,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    inputs: &PairInputs,
) -> AppResult<TreeUpdateBatchProof> {
    let (cms_padded, cv_deps_padded, pa, pi_in, is_dep) = build_padded_pair_arrays(inputs);
    let z = fiat_shamir::compute_z(
        &slot.old_root,
        &advanced.new_root,
        slot.start_index,
        1,
        &cms_padded,
        &cv_deps_padded,
        &pa,
        &pi_in,
        &is_dep,
    );
    let w = witness::build_n1(
        slot,
        advanced,
        &inputs.cm0,
        &inputs.cm1,
        &inputs.cv_dep0,
        &inputs.cv_dep1,
        z,
    );
    prover.prove(w).await
}

/// Encode the `TreeUpdateBatch` public-inputs struct that both spend and
/// swap calldata builders embed identically (actualCount = 1, single
/// active pair, rest zero-padded).
pub fn build_tu_pi_for_pair(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    arrays: PaddedBatchArrays,
) -> IMasp::TreeUpdateBatch {
    let (cms_padded, cv_deps_padded, pa, pi_in, is_dep) = arrays;
    build_tu_batch_pub_inputs(
        slot.start_index,
        &slot.old_root,
        &advanced.new_root,
        cms_padded,
        cv_deps_padded,
        pa,
        pi_in,
        is_dep,
        1,
    )
}
