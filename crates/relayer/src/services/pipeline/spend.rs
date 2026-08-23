use crate::adapters::abi::{IMasp, INativeAdapter};
use crate::adapters::calldata::{build_aux, build_proof, build_pub_inputs, build_tu_proof};
use crate::adapters::parse::parse_address;
use crate::domain::dto::{SpendKind, SubmitSpendPayload};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::EstimateResponse;
use crate::services::fee_quote::FeeQuoter;
use crate::services::gas_witness::{EntryPoint, GasWitness};
use crate::services::pipeline::common::{
    SPEND_LEAVES, SpendInputs, TransactBinding, build_tu_pi_for_spend, check_known_root,
    parse_spend_inputs, prove_spend, verify_transact_proof,
};
use crate::services::prover::{TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::submitter::{SubmissionReceipt, Submitter};
use crate::services::transact_verifier::TransactVerifier;
use crate::services::tree::{AdvancedState, MirrorSnapshot, ReservedSlot, TreeMirror};
use alloy::primitives::Address;
use alloy::sol_types::SolCall;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, instrument};

/// Native unshields do not go to the pool. `NativeAdapter` calls
/// `MASP.withdraw` itself, unwraps the proceeds and forwards them to
/// `pi.payer`, so it is both the SNARK's `recipient` and its `relayer`, and
/// the tx must be sent to the adapter address.
pub struct NativeRoute {
    pub address: Address,
    pub submitter: Arc<Submitter>,
}

pub struct SpendPipeline {
    pub chain_id: i64,
    pub mirror: Arc<Mutex<TreeMirror>>,
    /// Lock-free view of `mirror`, so `/chains` does not queue behind a
    /// submission holding the mutex through prove and confirmation.
    pub snapshot: Arc<MirrorSnapshot>,
    pub submitter: Arc<Submitter>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    pub fee_quoter: Arc<FeeQuoter>,
    pub gas_witness: Arc<GasWitness>,
    /// Built only for chains where `native_adapter_address` is configured.
    /// Without it, `withdrawNative` payloads are rejected.
    pub native: Option<Arc<NativeRoute>>,
    /// Checks the wallet's transact proof before the prover and the mirror
    /// lock are spent on it. `None` when the deployment shipped no transact
    /// verification key — see `ProverCfg::transact_vkey_path`.
    pub transact_verifier: Option<Arc<TransactVerifier>>,
}

impl SpendPipeline {
    /// `start_index` is deliberately absent from this span. It is the tree slot
    /// this caller's outputs land in, and pairing it with a timestamp maps a
    /// submission to its leaves. The chain publishes it anyway —
    /// `CommitmentTree.RootAdvanced(uint64 indexed startIndex, ...)` — so
    /// dropping it denies an observer nothing they cannot already read. It goes
    /// because a log line duplicating a public index is not worth the shape it
    /// gives an internal reader.
    #[instrument(skip_all, fields(chain_id = self.chain_id, kind = ?payload.kind))]
    pub async fn process(&self, payload: SubmitSpendPayload) -> AppResult<SubmissionReceipt> {
        self.validate(&payload)?;
        verify_transact_proof(
            self.transact_verifier.as_deref(),
            &payload.proof,
            &payload.pub_inputs,
            &payload.aux,
        )?;
        let inputs = parse_spend_inputs(&payload.pub_inputs)?;
        let entry = EntryPoint::from(payload.kind);
        let submitter = self.submitter_for(payload.kind)?;

        // Hold the mirror lock through reserve→prove→submit so concurrent
        // pipelines on this chain serialise cleanly.
        let mut mirror = self.mirror.lock().await;
        info!(
            leaf_count = mirror.committed_count(),
            "spend pipeline start"
        );
        check_known_root(&mirror, &payload.pub_inputs)?;
        let (slot, advanced) = mirror.reserve_and_advance_batch(&inputs.leaves())?;
        let tu_proof = match prove_spend(&self.prover, &slot, &advanced, &inputs).await {
            Ok(p) => p,
            Err(e) => return Err(mirror.unwind(SPEND_LEAVES, e)),
        };

        let calldata = encode_spend_calldata(&payload, &inputs, &slot, &advanced, &tu_proof)?;
        match submitter.submit(calldata).await {
            Ok(receipt) => {
                self.gas_witness.observe(entry, receipt.gas_used);
                info!(
                    entry = entry.as_str(),
                    tx_hash = %receipt.tx_hash,
                    gas_used = receipt.gas_used,
                    "spend submitted"
                );
                Ok(receipt)
            }
            Err(e) => Err(mirror.unwind(SPEND_LEAVES, e)),
        }
    }

    /// Fee quote for `/v1/spend/estimate`: prices this entry point's observed
    /// gas. Touches neither the tree mirror nor the prover, so it cannot stall
    /// a real submission.
    ///
    /// Takes a `SpendKind`, not a payload. The answer depends on the entry
    /// point alone, so asking for a full `SubmitSpendPayload` bought a shape
    /// check on a spend the caller may never submit, in exchange for a number
    /// that check could not change — see `EstimateSpendRequest`. There is
    /// consequently no pre-flight here at all: unlike the old
    /// `eth_estimateGas` path, an estimate does not tell a wallet whether its
    /// spend would revert on chain.
    pub async fn estimate(&self, kind: SpendKind) -> AppResult<EstimateResponse> {
        self.fee_quoter
            .quote_for_gas(self.gas_witness.gas_for(EntryPoint::from(kind)))
            .await
    }

    fn validate(&self, payload: &SubmitSpendPayload) -> AppResult<()> {
        validate_spend_shape(payload, self.binding(payload.kind)?, self.native_address())
    }

    /// The address the proof must name as `relayer`: this relayer's signer
    /// for pool-targeted calls, the adapter for a native unshield.
    fn binding(&self, kind: SpendKind) -> AppResult<TransactBinding> {
        let relayer = match kind {
            SpendKind::WithdrawNative => self.native_route()?.address,
            _ => self.submitter.signer_address,
        };
        Ok(TransactBinding {
            chain_id: self.chain_id,
            relayer,
        })
    }

    fn native_address(&self) -> Option<Address> {
        self.native.as_ref().map(|n| n.address)
    }

    fn native_route(&self) -> AppResult<&Arc<NativeRoute>> {
        self.native.as_ref().ok_or_else(|| {
            AppError::BadRequest(format!(
                "withdrawNative is not available on chain {}: no native_adapter_address configured",
                self.chain_id
            ))
        })
    }

    fn submitter_for(&self, kind: SpendKind) -> AppResult<Arc<Submitter>> {
        Ok(match kind {
            SpendKind::WithdrawNative => self.native_route()?.submitter.clone(),
            _ => self.submitter.clone(),
        })
    }
}

fn encode_spend_calldata(
    payload: &SubmitSpendPayload,
    _inputs: &SpendInputs,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    tu_proof: &TreeUpdateBatchProof,
) -> AppResult<Vec<u8>> {
    let p = build_proof(&payload.proof)?;
    let pi = build_pub_inputs(&payload.pub_inputs)?;
    let tp = build_tu_proof(tu_proof)?;
    let tpi = build_tu_pi_for_spend(slot, advanced, _inputs);
    let aux = build_aux(&payload.aux)?;
    let out = match payload.kind {
        SpendKind::Transfer => IMasp::transferCall {
            p,
            pi,
            tp,
            tpi,
            aux,
        }
        .abi_encode(),
        SpendKind::Withdraw => IMasp::withdrawCall {
            p,
            pi,
            tp,
            tpi,
            aux,
        }
        .abi_encode(),
        // Same argument tuple, different callee: the adapter forwards it to
        // `MASP.withdraw` and unwraps what comes back.
        SpendKind::WithdrawNative => INativeAdapter::withdrawNativeCall {
            p,
            pi,
            tp,
            tpi,
            aux,
        }
        .abi_encode(),
    };
    Ok(out)
}

fn validate_spend_shape(
    payload: &SubmitSpendPayload,
    binding: TransactBinding,
    native_adapter: Option<Address>,
) -> AppResult<()> {
    binding.check(&payload.pub_inputs)?;
    if payload.pub_inputs.public_in != 0 {
        return Err(AppError::BadRequest(
            "spend payload must have publicIn == 0".into(),
        ));
    }
    match payload.kind {
        SpendKind::Transfer => {
            if payload.pub_inputs.public_out != 0 {
                return Err(AppError::BadRequest(
                    "transfer requires publicOut == 0".into(),
                ));
            }
        }
        SpendKind::Withdraw => {
            if payload.pub_inputs.public_out == 0 {
                return Err(AppError::BadRequest(
                    "withdraw requires publicOut > 0".into(),
                ));
            }
        }
        SpendKind::WithdrawNative => {
            if payload.pub_inputs.public_out == 0 {
                return Err(AppError::BadRequest(
                    "withdrawNative requires publicOut > 0".into(),
                ));
            }
            // The adapter reverts `AdapterNotRecipient` otherwise; the ERC20
            // proceeds must land on it so it can unwrap them.
            let adapter = native_adapter.ok_or_else(|| {
                AppError::BadRequest("withdrawNative is not available on this chain".into())
            })?;
            let recipient = parse_address(&payload.pub_inputs.recipient)?;
            if recipient != adapter {
                return Err(AppError::BadRequest(format!(
                    "withdrawNative requires pi.recipient ({recipient}) to equal the native adapter ({adapter})"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::dto::{
        OutputAuxDto, PointDto, ProofDto, PubInputsDto, TRANSACT_IN, TRANSACT_OUT,
    };
    use std::array;

    const CHAIN_ID: i64 = 31337;

    fn relayer() -> Address {
        Address::from([0x99u8; 20])
    }

    fn adapter() -> Address {
        Address::from([0x88u8; 20])
    }

    fn payload(kind: SpendKind, public_out: u64) -> SubmitSpendPayload {
        let p = PointDto {
            x: "0".into(),
            y: "0".into(),
        };
        let aux = OutputAuxDto {
            clue_r: p.clone(),
            eph_pub: p.clone(),
            ciphertext: "0x".into(),
        };
        let recipient = match kind {
            SpendKind::WithdrawNative => adapter().to_string(),
            _ => "0x000000000000000000000000000000000000beef".into(),
        };
        let bound_relayer = match kind {
            SpendKind::WithdrawNative => adapter().to_string(),
            _ => relayer().to_string(),
        };
        SubmitSpendPayload {
            chain_id: CHAIN_ID,
            kind,
            proof: ProofDto {
                pi_a: ["0".into(), "0".into(), "1".into()],
                pi_b: [
                    ["0".into(), "0".into()],
                    ["0".into(), "0".into()],
                    ["1".into(), "0".into()],
                ],
                pi_c: ["0".into(), "0".into(), "1".into()],
            },
            pub_inputs: PubInputsDto {
                merkle_root: format!("0x{:0>64}", "0"),
                nullifier: array::from_fn(|i| format!("0x{:0>64}", i + 1)),
                out_cm: array::from_fn(|i| format!("0x{:0>64}", i + 10)),
                public_asset_id: 1,
                public_in: 0,
                public_out,
                in_cv: array::from_fn(|_| p.clone()),
                out_cv: array::from_fn(|_| p.clone()),
                out_cv_dep: array::from_fn(|_| p.clone()),
                recipient,
                chain_id: CHAIN_ID as u64,
                payer: "0x0000000000000000000000000000000000000000".into(),
                relayer: bound_relayer,
            },
            aux: array::from_fn(|_| aux.clone()),
        }
    }

    fn checked(
        kind: SpendKind,
        public_out: u64,
        mutate: impl FnOnce(&mut SubmitSpendPayload),
    ) -> AppResult<()> {
        let mut p = payload(kind, public_out);
        mutate(&mut p);
        let bound = match kind {
            SpendKind::WithdrawNative => adapter(),
            _ => relayer(),
        };
        validate_spend_shape(
            &p,
            TransactBinding {
                chain_id: CHAIN_ID,
                relayer: bound,
            },
            Some(adapter()),
        )
    }

    #[test]
    fn the_payload_shape_matches_the_deployed_circuit() {
        let p = payload(SpendKind::Transfer, 0);
        assert_eq!(p.pub_inputs.nullifier.len(), TRANSACT_IN);
        assert_eq!(p.pub_inputs.out_cm.len(), TRANSACT_OUT);
        assert_eq!(p.aux.len(), TRANSACT_OUT);
    }

    #[test]
    fn accepts_a_well_formed_transfer() {
        checked(SpendKind::Transfer, 0, |_| {}).unwrap();
    }

    #[test]
    fn accepts_a_well_formed_withdraw() {
        checked(SpendKind::Withdraw, 1_000, |_| {}).unwrap();
    }

    #[test]
    fn rejects_transfer_with_public_out() {
        assert!(checked(SpendKind::Transfer, 1, |_| {}).is_err());
    }

    #[test]
    fn rejects_withdraw_without_public_out() {
        assert!(checked(SpendKind::Withdraw, 0, |_| {}).is_err());
    }

    #[test]
    fn rejects_public_in_nonzero() {
        assert!(checked(SpendKind::Transfer, 0, |p| p.pub_inputs.public_in = 1).is_err());
    }

    /// The pipeline is selected by the envelope's chain id, but the SNARK is
    /// bound to the one inside pub_inputs — a mismatch always reverts.
    #[test]
    fn rejects_chain_id_mismatch() {
        let err = checked(SpendKind::Transfer, 0, |p| {
            p.pub_inputs.chain_id = CHAIN_ID as u64 + 1
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    /// The proof pins a relayer address; only that relayer can satisfy it.
    #[test]
    fn rejects_a_proof_bound_to_another_relayer() {
        let err = checked(SpendKind::Transfer, 0, |p| {
            p.pub_inputs.relayer = "0x000000000000000000000000000000000000dead".into()
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn rejects_duplicate_nullifiers() {
        let err = checked(SpendKind::Transfer, 0, |p| {
            p.pub_inputs.nullifier[2] = p.pub_inputs.nullifier[0].clone()
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    /// The adapter drives `MASP.withdraw` itself, so the proof must name it
    /// as relayer — this relayer's own signer would revert `AdapterNotRelayer`.
    #[test]
    fn native_withdraw_binds_to_the_adapter_not_the_signer() {
        checked(SpendKind::WithdrawNative, 1_000, |_| {}).unwrap();
        let err = checked(SpendKind::WithdrawNative, 1_000, |p| {
            p.pub_inputs.relayer = relayer().to_string()
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn native_withdraw_requires_the_adapter_as_recipient() {
        let err = checked(SpendKind::WithdrawNative, 1_000, |p| {
            p.pub_inputs.recipient = "0x000000000000000000000000000000000000beef".into()
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn native_withdraw_is_rejected_when_no_adapter_is_configured() {
        let p = payload(SpendKind::WithdrawNative, 1_000);
        let err = validate_spend_shape(
            &p,
            TransactBinding {
                chain_id: CHAIN_ID,
                relayer: adapter(),
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
