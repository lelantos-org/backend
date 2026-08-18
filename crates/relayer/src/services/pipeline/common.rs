//! Shared building blocks for single-transact pipelines (spend + swap).
//!
//! Both pipelines insert `TRANSACT_OUT` leaves, run the same
//! `tree_update_batch` SNARK over them, and differ only in: (a) which
//! contract they target, (b) how they encode the calldata, and (c) which
//! payload-shape checks they apply. This module owns everything they share.

use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{
    PaddedBatch, build_aux, build_pub_inputs, build_tu_batch_pub_inputs,
};
use crate::adapters::parse::{FieldRef, parse_address, parse_field};
use crate::domain::dto::{OutputAuxDto, PointDto, ProofDto, PubInputsDto, TRANSACT_OUT};
use crate::domain::error::{AppError, AppResult};
use crate::domain::fiat_shamir;
use crate::services::prover::{TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::transact_verifier::TransactVerifier;
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
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
        check_field_elements(pi)?;
        Ok(())
    }
}

/// Reject any wallet-supplied value that is not a canonical BN254 field
/// element.
///
/// This has to run before the mirror is touched. `out_cm` and `out_cv_dep`
/// feed `TreeMirror::reserve_and_advance_batch`, which hashes them with
/// Poseidon; a non-canonical one fails there, and if an earlier leaf in the
/// same batch already went in, the mirror is left ahead of the chain. The rest
/// are checked in the same pass because the contract's coefficient range check
/// would reject them anyway — better a 400 than a burnt Groth16.
pub fn check_field_elements(pi: &PubInputsDto) -> AppResult<()> {
    parse_field(&pi.merkle_root, FieldRef::Named("pubInputs.merkleRoot"))?;
    for (array, values) in [
        ("pubInputs.nullifier", pi.nullifier.as_slice()),
        ("pubInputs.outCm", pi.out_cm.as_slice()),
    ] {
        for (i, v) in values.iter().enumerate() {
            parse_field(v, FieldRef::Index(array, i))?;
        }
    }
    for (array, points) in [
        ("pubInputs.inCv", pi.in_cv.as_slice()),
        ("pubInputs.outCv", pi.out_cv.as_slice()),
        ("pubInputs.outCvDep", pi.out_cv_dep.as_slice()),
    ] {
        for (i, p) in points.iter().enumerate() {
            check_point(p, array, i)?;
        }
    }
    Ok(())
}

fn check_point(p: &PointDto, array: &str, i: usize) -> AppResult<()> {
    parse_field(&p.x, FieldRef::Coord(array, i, "x"))?;
    parse_field(&p.y, FieldRef::Coord(array, i, "y"))?;
    Ok(())
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

    /// The same leaves at the batch circuit's full width.
    pub fn padded(&self) -> PaddedBatch {
        PaddedBatch::from_spend(&self.cms, &self.cv_deps)
    }
}

pub fn parse_spend_inputs(pi: &PubInputsDto) -> AppResult<SpendInputs> {
    let mut cms = [FixedBytes::<32>::ZERO; SPEND_LEAVES];
    let mut cv_deps = [[U256::ZERO; 2]; SPEND_LEAVES];
    for i in 0..SPEND_LEAVES {
        cms[i] = parse_field(&pi.out_cm[i], FieldRef::Index("pubInputs.outCm", i))?;
        cv_deps[i] = [
            field_u256(
                &pi.out_cv_dep[i].x,
                FieldRef::Coord("pubInputs.outCvDep", i, "x"),
            )?,
            field_u256(
                &pi.out_cv_dep[i].y,
                FieldRef::Coord("pubInputs.outCvDep", i, "y"),
            )?,
        ];
    }
    Ok(SpendInputs { cms, cv_deps })
}

fn field_u256(s: &str, at: FieldRef<'_>) -> AppResult<U256> {
    Ok(U256::from_be_bytes(parse_field(s, at)?.0))
}

/// Verify the wallet's transact proof locally, before anything expensive.
///
/// Shape checks alone cannot tell a real proof from a fabricated one, so
/// without this a caller could spend the relayer's single-permit prover and the
/// chain's tree mutex on payloads that were always going to be rejected
/// on-chain. `None` means the deployment shipped no verification key, in which
/// case there is nothing to check against — see `ProverCfg::transact_vkey_path`.
pub fn verify_transact_proof(
    verifier: Option<&TransactVerifier>,
    proof: &ProofDto,
    pi: &PubInputsDto,
    aux: &[OutputAuxDto; TRANSACT_OUT],
) -> AppResult<()> {
    let Some(verifier) = verifier else {
        return Ok(());
    };
    verifier.verify(proof, &build_pub_inputs(pi)?, &build_aux(aux)?)
}

/// Reject a payload proved against a root this relayer has never held.
///
/// The pool would revert `StaleOldRoot`, which reaches the caller as an opaque
/// 502 after a full Groth16. Checked under the mirror lock, since that is what
/// makes the answer stable.
pub fn check_known_root(mirror: &TreeMirror, pi: &PubInputsDto) -> AppResult<()> {
    let root = parse_field(&pi.merkle_root, FieldRef::Named("pubInputs.merkleRoot"))?;
    if !mirror.knows_root(&root.0) {
        return Err(AppError::BadRequest(
            "pubInputs.merkleRoot is not a root this relayer has held recently; \
             refresh the tree state and re-prove"
                .into(),
        ));
    }
    Ok(())
}

/// Run the tree-update SNARK for one spend: derive `z`, build the witness,
/// hand off to the prover. Caller already holds the mirror lock.
pub async fn prove_spend(
    prover: &Arc<dyn TreeUpdateBatchProver>,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    inputs: &SpendInputs,
) -> AppResult<TreeUpdateBatchProof> {
    let z = fiat_shamir::compute_z(
        &slot.old_root,
        &advanced.new_root,
        slot.start_index,
        &inputs.padded(),
    );
    let w = witness::build_spend(slot, advanced, &inputs.cms, &inputs.cv_deps, z);
    prover.prove(w).await
}

/// Encode the `TreeUpdateBatch` public-inputs struct that both spend and swap
/// calldata builders embed identically.
pub fn build_tu_pi_for_spend(
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    inputs: &SpendInputs,
) -> IMasp::TreeUpdateBatch {
    build_tu_batch_pub_inputs(
        slot.start_index,
        &slot.old_root,
        &advanced.new_root,
        &inputs.padded(),
    )
}
