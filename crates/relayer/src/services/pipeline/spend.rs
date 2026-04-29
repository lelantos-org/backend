use crate::adapters::abi::IMasp;
use crate::adapters::calldata::{build_aux, build_proof, build_pub_inputs, build_tu_proof};
use crate::domain::dto::{SpendKind, SubmitSpendPayload};
use crate::domain::error::{AppError, AppResult};
use crate::services::fee_quote::FeeQuoter;
use crate::services::pipeline::common::{
    PAIR_LEAVES, PairInputs, build_padded_pair_arrays, build_tu_pi_for_pair, parse_pair_inputs,
    prove_pair,
};
use crate::services::prover::{TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::submitter::{SubmissionReceipt, Submitter};
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use alloy::sol_types::SolCall;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, instrument};

pub struct SpendPipeline {
    pub chain_id: i64,
    pub mirror: Arc<Mutex<TreeMirror>>,
    pub submitter: Arc<Submitter>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    pub fee_quoter: Arc<FeeQuoter>,
}

impl SpendPipeline {
    #[instrument(skip_all, fields(chain_id = self.chain_id, kind = ?payload.kind, start_index))]
    pub async fn process(&self, payload: SubmitSpendPayload) -> AppResult<SubmissionReceipt> {
        validate_spend_shape(&payload)?;
        let inputs = parse_pair_inputs(&payload.pub_inputs)?;

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
            Err(e) => {
                error!(error = %e, "prove failed; rolling back mirror");
                mirror.rollback(PAIR_LEAVES)?;
                return Err(e);
            }
        };

        let (calldata, entry) =
            encode_spend_calldata(&payload, &inputs, &slot, &advanced, &tu_proof)?;
        match self.submitter.submit(calldata).await {
            Ok(receipt) => {
                info!(
                    entry,
                    tx_hash = %receipt.tx_hash,
                    gas_used = receipt.gas_used,
                    "spend submitted"
                );
                Ok(receipt)
            }
            Err(e) => {
                error!(error = %e, "submit failed; rolling back mirror");
                mirror.rollback(PAIR_LEAVES)?;
                Err(e)
            }
        }
    }

    /// Build calldata as `process()` would, but never mutate the tree
    /// mirror or submit. Used by `/v1/spend/estimate`.
    ///
    /// The mirror lock is held only for the `preview_advance` call (which
    /// inserts then immediately rolls back); the CPU-heavy SNARK prove
    /// runs lock-free so real submits are not blocked.
    pub async fn dry_build_calldata(&self, payload: SubmitSpendPayload) -> AppResult<Vec<u8>> {
        validate_spend_shape(&payload)?;
        let inputs = parse_pair_inputs(&payload.pub_inputs)?;
        let (slot, advanced) = {
            let mut mirror = self.mirror.lock().await;
            mirror.preview_advance(*inputs.cm0, *inputs.cm1, &inputs.cv_dep0, &inputs.cv_dep1)?
        };
        let tu_proof = prove_pair(&self.prover, &slot, &advanced, &inputs).await?;
        let (calldata, _) = encode_spend_calldata(&payload, &inputs, &slot, &advanced, &tu_proof)?;
        Ok(calldata)
    }
}

fn encode_spend_calldata(
    payload: &SubmitSpendPayload,
    inputs: &PairInputs,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    tu_proof: &TreeUpdateBatchProof,
) -> AppResult<(Vec<u8>, &'static str)> {
    let p = build_proof(&payload.proof2x2)?;
    let pi = build_pub_inputs(&payload.pub_inputs)?;
    let tp = build_tu_proof(tu_proof)?;
    let tpi = build_tu_pi_for_pair(slot, advanced, build_padded_pair_arrays(inputs));
    let aux = build_aux(&payload.aux)?;
    let out = match payload.kind {
        SpendKind::Transfer => (
            IMasp::transferCall {
                p,
                pi,
                tp,
                tpi,
                aux,
            }
            .abi_encode(),
            "transfer",
        ),
        SpendKind::Withdraw => (
            IMasp::withdrawCall {
                p,
                pi,
                tp,
                tpi,
                aux,
            }
            .abi_encode(),
            "withdraw",
        ),
        SpendKind::WithdrawNative => (
            IMasp::withdrawNativeCall {
                p,
                pi,
                tp,
                tpi,
                aux,
            }
            .abi_encode(),
            "withdrawNative",
        ),
    };
    Ok(out)
}

fn validate_spend_shape(payload: &SubmitSpendPayload) -> AppResult<()> {
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
