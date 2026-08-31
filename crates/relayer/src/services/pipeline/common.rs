//! Shared building blocks for single-transact pipelines (spend + swap).
//!
//! Both pipelines insert `TRANSACT_OUT` leaves and run the same
//! `tree_update_batch` SNARK over them, differing only in which contract they
//! target, how they encode the calldata, and which payload-shape checks they
//! apply. This module owns what they share.

use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{
    PaddedBatch, build_aux, build_pub_inputs, build_tu_batch_pub_inputs,
};
use crate::adapters::parse::{FieldRef, parse_address, parse_field};
use crate::domain::dto::{OutputAuxDto, PointDto, ProofDto, PubInputsDto, TRANSACT_OUT};
use crate::domain::error::{AppError, AppResult};
use crate::domain::fiat_shamir;
use crate::domain::responses::EstimateResponse;
use crate::domain::units::Scale;
use crate::services::asset_registry::AssetRegistry;
use crate::services::fee_quote::FeeQuoter;
use crate::services::prover::{Priority, TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::shielded_fee::ShieldedFeeChecker;
use crate::services::transact_verifier::TransactVerifier;
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use crate::services::witness;
use alloy::primitives::{Address, FixedBytes, U256};
use std::sync::Arc;
use tracing::info;

/// A spend inserts one leaf per transact output.
pub const SPEND_LEAVES: usize = TRANSACT_OUT;

/// What a payload's transact proof must be bound to for this relayer to land it.
/// Named fields rather than positional arguments, since both are checked against
/// wallet-supplied values and `chain_id` and `relayer` are easy to transpose.
#[derive(Debug, Clone, Copy)]
pub struct TransactBinding {
    pub chain_id: i64,
    /// Address the proof must name as `relayer`: usually this relayer's signer,
    /// and for a native unshield the `NativeAdapter`, which drives `MASP.withdraw`
    /// itself and is therefore the pool's caller.
    pub relayer: Address,
}

impl TransactBinding {
    /// Reject payloads that cannot land, before the prover runs. Each case is an
    /// on-chain revert that would otherwise cost a multi-second Groth16 first.
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
        // Pairwise over the whole input shape: the circuit constrains every pair,
        // so any repeat is a double-spend the pool would reject.
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
/// Runs before the mirror is touched. `out_cm` and `out_cv_dep` feed
/// `TreeMirror::reserve_and_advance_batch`, which hashes them with Poseidon; a
/// non-canonical value fails there, and an earlier leaf in the same batch would
/// already have gone in, leaving the mirror ahead of the chain. The rest are
/// checked in the same pass because the contract's coefficient range check would
/// reject them anyway, and a 400 is cheaper than a wasted Groth16.
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

/// Parsed leg-1 output commitments and value commitments, ready to feed the tree
/// mirror, the Fiat-Shamir transcript and the SNARK witness builder.
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
/// Shape checks alone cannot distinguish a real proof from a fabricated one, so
/// without this a caller could consume the relayer's single-permit prover and the
/// chain's tree mutex on payloads bound to be rejected on chain. `None` means the
/// deployment shipped no verification key; see `ProverCfg::transact_vkey_path`.
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
/// 502 after a full Groth16. Checked under the mirror lock, which makes the
/// answer stable.
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
    prover.prove(w, Priority::Spend).await
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

/// Everything both pipelines need in order to quote and to charge.
///
/// Spend and swap differ in which contract they target and how they encode
/// calldata, not in how a fee is priced or collected. Passing one struct keeps
/// that true: a new field is added once and reaches both paths.
#[derive(Clone, Copy)]
pub struct FeeContext<'a> {
    pub chain_id: i64,
    pub fee_quoter: &'a FeeQuoter,
    pub assets: &'a AssetRegistry,
    /// `None` on a chain that collects no fee, where the relayer pays gas from its
    /// own signer.
    pub shielded_fee: Option<&'a ShieldedFeeChecker>,
}

impl FeeContext<'_> {
    /// Quote `gas_used`, then join the result to the asset registry so a client
    /// gets everything it needs to build a fee note in one call.
    ///
    /// `FeeQuoter` prices tokens by ERC-20 address and knows nothing about MASP
    /// asset ids or scales, while the registry knows both and nothing about
    /// prices. The pipeline layer joins the two.
    pub async fn quote(&self, gas_used: u64) -> AppResult<EstimateResponse> {
        let mut estimate = self.fee_quoter.quote_for_gas(gas_used).await?;
        estimate.shielded_fee_address = self.shielded_fee.map(|c| c.address().to_string());

        let registered = self.assets.for_chain(self.chain_id).await?;
        for quote in &mut estimate.fees {
            // A fee token with no registered asset is left undecorated rather than
            // dropped: the amount is still useful on a chain where the indexer has
            // not caught up and no note can be built.
            let Some(row) = parse_address(&quote.token_address)
                .ok()
                .and_then(|t| registered.iter().find(|a| a.token_address() == Some(t)))
            else {
                continue;
            };
            // A yield asset whose index has not been polled yet yields no
            // `rate`, so the quote is left undecorated rather than converted at
            // `scale` — which would name a circuit amount that underpays.
            let (Some(rate), Ok(amount)) = (
                Scale::from_decimal(&row.scale).and_then(|s| row.rate(s)),
                U256::from_str_radix(&quote.amount, 10),
            ) else {
                continue;
            };
            quote.asset_id = Some(row.asset_id_u64);
            quote.scale = Some(row.scale.to_string());
            quote.circuit_amount = Some(rate.to_circuit_ceil(amount).to_string());
        }
        Ok(estimate)
    }

    /// Enforce the shielded fee, where this chain collects one.
    ///
    /// Runs after the proof check, so the public inputs a fee is bound to are known
    /// good, and before the tree-mirror lock, so a caller who underpays does not
    /// park every other submission on the chain.
    pub async fn charge(
        &self,
        pi: &PubInputsDto,
        aux: &[OutputAuxDto; TRANSACT_OUT],
        gas_used: u64,
    ) -> AppResult<()> {
        let Some(checker) = self.shielded_fee else {
            return Ok(());
        };
        let paid = checker.require(pi, aux, gas_used).await?;
        // The asset and the amount, with nothing tying them to this payer: the
        // output index would identify which slot of which submission holds the
        // note, which is the link the shielded fee exists to avoid.
        info!(
            chain_id = self.chain_id,
            asset_id = paid.asset_id,
            base_amount = %paid.base_amount,
            "shielded fee accepted"
        );
        Ok(())
    }
}
