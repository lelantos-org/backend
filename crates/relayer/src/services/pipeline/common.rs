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
use crate::domain::responses::{EstimateResponse, FeeQuote};
use crate::domain::units::Scale;
use crate::repositories::assets::AssetRow;
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

/// One quote in, one quote per REGISTERED ASSET out.
///
/// `FeeQuoter` is keyed by ERC-20 address and the registry by asset id, and that
/// relation is one-to-many: a yield asset is registered alongside the plain
/// asset it shadows and shares its token, differing only in the venue binding.
/// Decorating each quote in place could therefore name only one of them —
/// whichever the registry happened to list first — and a client asking to pay in
/// the other is told the relayer never quoted it. That makes every yield id
/// unusable: a fee note is denominated in the asset being moved, so there is no
/// other asset to fall back to.
///
/// The price is per token and identical across the ids sharing it. Only the id,
/// the scale and the circuit amount differ, since a yield unit is worth
/// `gross / supply` rather than `scale`.
fn decorate_fees(fees: Vec<FeeQuote>, registered: &[AssetRow]) -> Vec<FeeQuote> {
    let mut out: Vec<FeeQuote> = Vec::with_capacity(fees.len());
    for quote in fees {
        let token = parse_address(&quote.token_address).ok();
        let amount = U256::from_str_radix(&quote.amount, 10).ok();
        let (Some(token), Some(amount)) = (token, amount) else {
            // Nothing to join on. Kept undecorated for display, as an
            // unregistered token is.
            out.push(quote);
            continue;
        };

        let before = out.len();
        for row in registered
            .iter()
            .filter(|a| a.token_address() == Some(token))
        {
            // A yield asset whose index has not been polled yet yields no
            // `rate`, so it contributes no quote rather than one converted at
            // `scale` — which would name a circuit amount that underpays.
            let Some(rate) = Scale::from_decimal(&row.scale).and_then(|s| row.rate(s)) else {
                continue;
            };
            out.push(FeeQuote {
                asset_id: Some(row.asset_id_u64),
                scale: Some(row.scale.to_string()),
                circuit_amount: Some(rate.to_circuit_ceil(amount).to_string()),
                ..quote.clone()
            });
        }

        // A fee token with no registered asset — or none the indexer can price
        // yet — is left undecorated rather than dropped: the amount is still
        // useful on a chain where the indexer has not caught up and no note can
        // be built.
        if out.len() == before {
            out.push(quote);
        }
    }
    out
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
        estimate.fees = decorate_fees(estimate.fees, &registered);
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

#[cfg(test)]
mod tests {
    use super::{FeeQuote, decorate_fees};
    use crate::repositories::assets::AssetRow;

    /// A priced token, before the registry is joined to it.
    fn quote(token: u8, amount: &str) -> FeeQuote {
        FeeQuote {
            token_symbol: "mDAI".to_string(),
            token_address: format!("0x{}", hex::encode([token; 20])),
            decimals: 18,
            amount: amount.to_string(),
            asset_id: None,
            scale: None,
            circuit_amount: None,
        }
    }

    /// A plain asset: no venue, so it prices at `scale` forever.
    fn row(asset_id: i64, token: u8) -> AssetRow {
        AssetRow {
            asset_id_u64: asset_id,
            token: vec![token; 20],
            scale: "1000000000000".parse().expect("scale"),
            decimals: Some(18),
            symbol: None,
            deposit_bps: None,
            withdraw_bps: None,
            venue: None,
            gross: None,
            total_normalized: None,
            accrued_fee_normalized: None,
            halted: None,
            index_ray: None,
            perf_bps: None,
            buffer_bps: None,
        }
    }

    /// The yield id registered alongside `row`'s asset, sharing its token. Its
    /// venue has earned, so a unit is worth `1.1 * scale`.
    fn yield_row(asset_id: i64, token: u8) -> AssetRow {
        AssetRow {
            venue: Some(vec![0xaa; 20]),
            gross: Some("1100000000000000".parse().expect("gross")),
            total_normalized: Some("1000".parse().expect("supply")),
            accrued_fee_normalized: Some("0".parse().expect("fee")),
            halted: Some(false),
            index_ray: Some("1100000000000000000000000000".parse().expect("index")),
            ..row(asset_id, token)
        }
    }

    fn ids(fees: &[FeeQuote]) -> Vec<Option<i64>> {
        fees.iter().map(|f| f.asset_id).collect()
    }

    /// The regression this function exists for.
    ///
    /// A yield id shares its plain id's ERC-20, so a join that took the first
    /// registered asset at that address quoted only the plain one. A wallet
    /// depositing into the yield id was then told the relayer had quoted no
    /// amount for it — and since a fee note is denominated in the asset being
    /// moved, that made the id unusable rather than merely more expensive.
    #[test]
    fn both_ids_sharing_a_token_are_quoted() {
        let registered = [row(2, 0xbb), yield_row(5, 0xbb)];
        let out = decorate_fees(vec![quote(0xbb, "1100000000000000")], &registered);

        assert_eq!(ids(&out), vec![Some(2), Some(5)]);
        // One price, two conversions: the yield unit is worth more, so covering
        // the same cost takes fewer of them.
        let plain: u128 = out[0]
            .circuit_amount
            .as_ref()
            .expect("plain")
            .parse()
            .expect("n");
        let earning: u128 = out[1]
            .circuit_amount
            .as_ref()
            .expect("yield")
            .parse()
            .expect("n");
        assert!(earning < plain, "a yield unit covers more than a plain one");
        assert_eq!(out[0].amount, out[1].amount, "the token price is the same");
    }

    /// The single-asset case is unchanged: one quote in, one out.
    #[test]
    fn a_token_with_one_asset_yields_one_quote() {
        let out = decorate_fees(vec![quote(0xbb, "1000")], &[row(2, 0xbb)]);
        assert_eq!(ids(&out), vec![Some(2)]);
    }

    /// An unregistered token keeps its quote, undecorated: the amount is still
    /// worth displaying where the indexer has not caught up.
    #[test]
    fn an_unregistered_token_is_kept_undecorated() {
        let out = decorate_fees(vec![quote(0xcc, "1000")], &[row(2, 0xbb)]);
        assert_eq!(ids(&out), vec![None]);
        assert_eq!(out.len(), 1);
    }

    /// A yield asset the poller has not reached has no rate, so it contributes
    /// no quote — but the plain id at the same address still does.
    #[test]
    fn an_unpolled_yield_asset_is_skipped_not_fatal() {
        let mut unpolled = yield_row(5, 0xbb);
        unpolled.gross = None;
        let out = decorate_fees(vec![quote(0xbb, "1000")], &[row(2, 0xbb), unpolled]);
        assert_eq!(ids(&out), vec![Some(2)]);
    }

    /// When the only asset at an address cannot be priced, the quote survives
    /// undecorated rather than vanishing from the response.
    #[test]
    fn a_token_whose_only_asset_is_unpriceable_keeps_its_quote() {
        let mut unpolled = yield_row(5, 0xbb);
        unpolled.gross = None;
        let out = decorate_fees(vec![quote(0xbb, "1000")], &[unpolled]);
        assert_eq!(ids(&out), vec![None]);
    }
}
