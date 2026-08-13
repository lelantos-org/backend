use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{build_aux, build_proof, build_pub_inputs, build_tu_proof};
use crate::domain::dto::{SpendKind, SubmitSpendPayload};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::EstimateResponse;
use crate::services::fee_quote::FeeQuoter;
use crate::services::gas_witness::{EntryPoint, GasWitness};
use crate::services::pipeline::common::{
    PAIR_LEAVES, PairInputs, TransactBinding, build_padded_pair_arrays, build_tu_pi_for_pair,
    parse_pair_inputs, prove_pair,
};
use crate::services::prover::{TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::submitter::{SubmissionReceipt, Submitter};
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use alloy::sol_types::SolCall;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, instrument};

pub struct SpendPipeline {
    pub chain_id: i64,
    pub mirror: Arc<Mutex<TreeMirror>>,
    pub submitter: Arc<Submitter>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    pub fee_quoter: Arc<FeeQuoter>,
    pub gas_witness: Arc<GasWitness>,
}

impl SpendPipeline {
    #[instrument(skip_all, fields(chain_id = self.chain_id, kind = ?payload.kind, start_index))]
    pub async fn process(&self, payload: SubmitSpendPayload) -> AppResult<SubmissionReceipt> {
        self.validate(&payload)?;
        let inputs = parse_pair_inputs(&payload.pub_inputs)?;
        let entry = EntryPoint::from(payload.kind);

        // Hold the mirror lock through reserve→prove→submit so concurrent
        // pipelines on this chain serialise cleanly.
        let mut mirror = self.mirror.lock().await;
        info!(
            leaf_count = mirror.committed_count(),
            "spend pipeline start"
        );
        let (slot, advanced) = mirror.reserve_and_advance(
            *inputs.cm0,
            *inputs.cm1,
            &inputs.cv_dep0,
            &inputs.cv_dep1,
        )?;
        tracing::Span::current().record("start_index", slot.start_index);

        let tu_proof = match prove_pair(&self.prover, &slot, &advanced, &inputs).await {
            Ok(p) => p,
            Err(e) => return Err(mirror.unwind(PAIR_LEAVES, e)),
        };

        let calldata = encode_spend_calldata(&payload, &inputs, &slot, &advanced, &tu_proof)?;
        match self.submitter.submit(calldata).await {
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
            Err(e) => Err(mirror.unwind(PAIR_LEAVES, e)),
        }
    }

    /// Fee quote for `/v1/spend/estimate`. Validates the payload shape, then
    /// prices this entry point's observed gas — it neither touches the tree
    /// mirror nor proves, so it cannot stall a real submission.
    ///
    /// Unlike the old `eth_estimateGas` path, this does not detect a payload
    /// that would revert on-chain; the shape checks below are the only
    /// pre-flight the estimate performs.
    pub async fn estimate(&self, payload: SubmitSpendPayload) -> AppResult<EstimateResponse> {
        self.validate(&payload)?;
        let entry = EntryPoint::from(payload.kind);
        self.fee_quoter
            .quote_for_gas(self.gas_witness.gas_for(entry))
            .await
    }

    fn validate(&self, payload: &SubmitSpendPayload) -> AppResult<()> {
        validate_spend_shape(payload, self.binding())
    }

    fn binding(&self) -> TransactBinding {
        TransactBinding {
            chain_id: self.chain_id,
            relayer: self.submitter.signer_address,
        }
    }
}

fn encode_spend_calldata(
    payload: &SubmitSpendPayload,
    inputs: &PairInputs,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    tu_proof: &TreeUpdateBatchProof,
) -> AppResult<Vec<u8>> {
    let p = build_proof(&payload.proof2x2)?;
    let pi = build_pub_inputs(&payload.pub_inputs)?;
    let tp = build_tu_proof(tu_proof)?;
    let tpi = build_tu_pi_for_pair(slot, advanced, build_padded_pair_arrays(inputs));
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
        SpendKind::WithdrawNative => IMasp::withdrawNativeCall {
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

fn validate_spend_shape(payload: &SubmitSpendPayload, binding: TransactBinding) -> AppResult<()> {
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
        SpendKind::Withdraw | SpendKind::WithdrawNative => {
            if payload.pub_inputs.public_out == 0 {
                return Err(AppError::BadRequest(
                    "withdraw/withdrawNative require publicOut > 0".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::dto::{OutputAuxDto, PointDto, ProofDto, PubInputsDto};
    use alloy::primitives::Address;

    const CHAIN_ID: i64 = 31337;

    fn relayer() -> Address {
        Address::from([0x99u8; 20])
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
        SubmitSpendPayload {
            chain_id: CHAIN_ID,
            kind,
            proof2x2: ProofDto {
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
                nullifier: [format!("0x{:0>64}", "1"), format!("0x{:0>64}", "2")],
                out_cm: [format!("0x{:0>64}", "3"), format!("0x{:0>64}", "4")],
                public_asset_id: 1,
                public_in: 0,
                public_out,
                in_cv: [p.clone(), p.clone()],
                out_cv: [p.clone(), p.clone()],
                out_cv_dep: [p.clone(), p],
                recipient: "0x000000000000000000000000000000000000beef".into(),
                chain_id: CHAIN_ID as u64,
                payer: "0x0000000000000000000000000000000000000000".into(),
                relayer: relayer().to_string(),
            },
            aux: [aux.clone(), aux],
        }
    }

    fn checked(
        kind: SpendKind,
        public_out: u64,
        mutate: impl FnOnce(&mut SubmitSpendPayload),
    ) -> AppResult<()> {
        let mut p = payload(kind, public_out);
        mutate(&mut p);
        validate_spend_shape(
            &p,
            TransactBinding {
                chain_id: CHAIN_ID,
                relayer: relayer(),
            },
        )
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
            p.pub_inputs.nullifier[1] = p.pub_inputs.nullifier[0].clone()
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
