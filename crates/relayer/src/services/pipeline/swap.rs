use crate::adapters::abi::ISwapWrapper;
use crate::adapters::calldata::{
    build_aux, build_deposit_request, build_one_aux, build_proof, build_pub_inputs, build_tu_proof,
};
use crate::adapters::parse::{parse_address, parse_hex_bytes, parse_u256};
use crate::domain::dto::SubmitSwapPayload;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::EstimateResponse;
use crate::services::fee_quote::FeeQuoter;
use crate::services::gas_witness::{EntryPoint, GasWitness};
use crate::services::pipeline::common::{
    SPEND_LEAVES, SpendInputs, TransactBinding, build_padded_spend_arrays, build_tu_pi_for_spend,
    parse_spend_inputs, prove_spend,
};
use crate::services::prover::{TreeUpdateBatchProof, TreeUpdateBatchProver};
use crate::services::submitter::{SubmissionReceipt, Submitter};
use crate::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use alloy::primitives::Address;
use alloy::sol_types::SolCall;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, instrument};

/// Per-chain swap pipeline. Mirrors `SpendPipeline` end-to-end except:
///   - the leg-1 SNARK PIs travel inside `SwapWrapper.SwapArgs.pi_w`
///     instead of the bare MASP entry point;
///   - calldata targets the wrapper, not the pool;
///   - leg-2 escrow data (`deposit_d`, `aux_d`) ride along in the same
///     calldata blob — no separate Permit2 pull, no SNARK at submit.
///
/// Shares the per-chain `TreeMirror` mutex with `SpendPipeline` so spend
/// and swap submissions serialise against each other.
pub struct SwapPipeline {
    pub chain_id: i64,
    pub mirror: Arc<Mutex<TreeMirror>>,
    /// Submitter target MUST be the SwapWrapper address, not the MASP.
    pub submitter: Arc<Submitter>,
    pub prover: Arc<dyn TreeUpdateBatchProver>,
    /// Cached for shape validation: `pi_w.recipient` + `deposit_d.payer`
    /// must both equal this. Wrapper enforces on-chain too, but rejecting
    /// here gives a 400 instead of a wasted Groth16 + revert.
    pub wrapper_address: Address,
    pub fee_quoter: Arc<FeeQuoter>,
    pub gas_witness: Arc<GasWitness>,
}

impl SwapPipeline {
    #[instrument(
        skip_all,
        fields(chain_id = self.chain_id, adapter = %payload.swap.adapter, start_index),
    )]
    pub async fn process(&self, payload: SubmitSwapPayload) -> AppResult<SubmissionReceipt> {
        self.validate(&payload)?;
        let inputs = parse_spend_inputs(&payload.pub_inputs)?;

        let mut mirror = self.mirror.lock().await;
        info!(
            leaf_count = mirror.committed_count(),
            token_in = %payload.swap.token_in,
            token_out = %payload.swap.token_out,
            "swap pipeline start"
        );
        let (slot, advanced) = mirror.reserve_and_advance_batch(&inputs.leaves())?;
        tracing::Span::current().record("start_index", slot.start_index);

        let tu_proof = match prove_spend(&self.prover, &slot, &advanced, &inputs).await {
            Ok(p) => p,
            Err(e) => return Err(mirror.unwind(SPEND_LEAVES, e)),
        };

        let calldata = encode_swap_calldata(&payload, &inputs, &slot, &advanced, &tu_proof)?;
        match self.submitter.submit(calldata).await {
            Ok(receipt) => {
                self.gas_witness.observe(EntryPoint::Swap, receipt.gas_used);
                info!(
                    tx_hash = %receipt.tx_hash,
                    gas_used = receipt.gas_used,
                    "swap submitted"
                );
                Ok(receipt)
            }
            Err(e) => Err(mirror.unwind(SPEND_LEAVES, e)),
        }
    }

    /// Fee quote for `/v1/swap/estimate`. See `SpendPipeline::estimate` — no
    /// mirror access, no prove.
    pub async fn estimate(&self, payload: SubmitSwapPayload) -> AppResult<EstimateResponse> {
        self.validate(&payload)?;
        self.fee_quoter
            .quote_for_gas(self.gas_witness.gas_for(EntryPoint::Swap))
            .await
    }

    fn validate(&self, payload: &SubmitSwapPayload) -> AppResult<()> {
        let binding = TransactBinding {
            chain_id: self.chain_id,
            relayer: self.submitter.signer_address,
        };
        validate_swap_shape(payload, self.wrapper_address, binding)
    }
}

fn encode_swap_calldata(
    payload: &SubmitSwapPayload,
    inputs: &SpendInputs,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    tu_proof: &TreeUpdateBatchProof,
) -> AppResult<Vec<u8>> {
    let p_w = build_proof(&payload.proof)?;
    let pi_w = build_pub_inputs(&payload.pub_inputs)?;
    let tp_w = build_tu_proof(tu_proof)?;
    let tpi_w = build_tu_pi_for_spend(slot, advanced, build_padded_spend_arrays(inputs));
    let aux_w = build_aux(&payload.aux)?;
    let deposit_d = build_deposit_request(&payload.swap.deposit_d)?;
    let aux_d = build_one_aux(&payload.swap.aux_d)?;
    let route_bytes = parse_hex_bytes(&payload.swap.route, "route")?;
    let deadline = match &payload.swap.deadline {
        Some(s) => parse_u256(s)?,
        None => alloy::primitives::U256::MAX,
    };
    Ok(ISwapWrapper::swapCall {
        a: ISwapWrapper::SwapArgs {
            p_w,
            pi_w,
            tp_w,
            tpi_w,
            aux_w,
            deposit_d,
            aux_d,
            adapter: parse_address(&payload.swap.adapter)?,
            route: route_bytes,
            deadline,
            tokenIn: parse_address(&payload.swap.token_in)?,
            tokenOut: parse_address(&payload.swap.token_out)?,
            amountIn: parse_u256(&payload.swap.amount_in)?,
            minOut: parse_u256(&payload.swap.min_out)?,
        },
    }
    .abi_encode())
}

fn validate_swap_shape(
    p: &SubmitSwapPayload,
    wrapper: Address,
    binding: TransactBinding,
) -> AppResult<()> {
    binding.check(&p.pub_inputs)?;
    // Leg 2 is chain-bound separately: it rides in the same calldata but the
    // wrapper escrows it into MASP under its own `chainId` field.
    if p.swap.deposit_d.chain_id != binding.chain_id as u64 {
        return Err(AppError::BadRequest(format!(
            "deposit_d.chainId ({}) must equal the request chainId ({})",
            p.swap.deposit_d.chain_id, binding.chain_id
        )));
    }
    // Leg 1 is structurally a withdraw: shielded note(s) → public token to
    // the wrapper. The transact SNARK enforces conservation; we just block
    // the obviously-wrong shapes early.
    if p.pub_inputs.public_in != 0 {
        return Err(AppError::BadRequest(
            "swap payload must have publicIn == 0".into(),
        ));
    }
    if p.pub_inputs.public_out == 0 {
        return Err(AppError::BadRequest(
            "swap payload must have publicOut > 0".into(),
        ));
    }
    let pi_recipient = parse_address(&p.pub_inputs.recipient)?;
    if pi_recipient != wrapper {
        return Err(AppError::BadRequest(format!(
            "pi.recipient ({pi_recipient}) must equal swap_wrapper_address ({wrapper})"
        )));
    }
    let deposit_payer = parse_address(&p.swap.deposit_d.payer)?;
    if deposit_payer != wrapper {
        return Err(AppError::BadRequest(format!(
            "deposit_d.payer ({deposit_payer}) must equal swap_wrapper_address ({wrapper})"
        )));
    }
    if p.swap.deposit_d.public_in == 0 {
        return Err(AppError::BadRequest(
            "deposit_d.publicIn must be > 0".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::dto::{
        DepositRequestDto, OutputAuxDto, PointDto, ProofDto, PubInputsDto, SwapBlob,
    };
    use std::array;

    fn fake_proof() -> ProofDto {
        ProofDto {
            pi_a: ["0".into(), "0".into(), "1".into()],
            pi_b: [
                ["0".into(), "0".into()],
                ["0".into(), "0".into()],
                ["1".into(), "0".into()],
            ],
            pi_c: ["0".into(), "0".into(), "1".into()],
        }
    }

    fn one_aux() -> OutputAuxDto {
        let p = PointDto {
            x: "0".into(),
            y: "0".into(),
        };
        OutputAuxDto {
            clue_r: p.clone(),
            eph_pub: p,
            ciphertext: "0x".into(),
        }
    }

    fn fake_pub_inputs(recipient: &str, public_out: u64) -> PubInputsDto {
        let p = PointDto {
            x: "0".into(),
            y: "0".into(),
        };
        PubInputsDto {
            merkle_root: format!("0x{:0>64}", "0"),
            nullifier: array::from_fn(|i| format!("0x{:0>64}", i + 1)),
            out_cm: array::from_fn(|i| format!("0x{:0>64}", i + 10)),
            public_asset_id: 1,
            public_in: 0,
            public_out,
            in_cv: array::from_fn(|_| p.clone()),
            out_cv: array::from_fn(|_| p.clone()),
            out_cv_dep: array::from_fn(|_| p.clone()),
            recipient: recipient.to_string(),
            chain_id: 31337,
            payer: "0x0000000000000000000000000000000000000000".into(),
            relayer: "0x0000000000000000000000000000000000000000".into(),
        }
    }

    fn fake_deposit(payer: &str, public_in: u64) -> DepositRequestDto {
        let zero = format!("0x{:0>64}", "0");
        DepositRequestDto {
            chain_id: 31337,
            public_asset_id: 2,
            public_in,
            payer: payer.to_string(),
            recipient: "0x000000000000000000000000000000000000beef".into(),
            out_cm: format!("0x{:0>64}", "5"),
            cv_dep: [zero.clone(), zero.clone()],
            rcv: zero,
        }
    }

    fn fake_payload(wrapper: &str) -> SubmitSwapPayload {
        SubmitSwapPayload {
            chain_id: 31337,
            proof: fake_proof(),
            pub_inputs: fake_pub_inputs(wrapper, 1_000),
            aux: array::from_fn(|_| one_aux()),
            swap: SwapBlob {
                adapter: "0x0000000000000000000000000000000000000001".into(),
                // 64B single-hop: abi.encode(uint24 fee, uint160 sqrtPriceLimitX96).
                route:
                    "0x00000000000000000000000000000000000000000000000000000000000000640000000000000000000000000000000000000000000000000000000000000000"
                        .into(),
                deposit_d: fake_deposit(wrapper, 990),
                aux_d: one_aux(),
                token_in: "0x0000000000000000000000000000000000000111".into(),
                token_out: "0x0000000000000000000000000000000000000222".into(),
                amount_in: "1000".into(),
                min_out: "990".into(),
                deadline: None,
            },
        }
    }

    fn wrapper() -> Address {
        Address::from([0x77u8; 20])
    }

    const CHAIN_ID: i64 = 31337;

    fn relayer() -> Address {
        Address::from([0x99u8; 20])
    }

    /// Payload as a well-behaved wallet would send it, plus a mutation.
    fn checked(mutate: impl FnOnce(&mut SubmitSwapPayload)) -> AppResult<()> {
        let w = wrapper();
        let mut p = fake_payload(&w.to_string());
        p.pub_inputs.relayer = relayer().to_string();
        mutate(&mut p);
        validate_swap_shape(
            &p,
            w,
            TransactBinding {
                chain_id: CHAIN_ID,
                relayer: relayer(),
            },
        )
    }

    #[test]
    fn validate_accepts_well_formed_payload() {
        checked(|_| {}).unwrap();
    }

    #[test]
    fn validate_rejects_public_in_nonzero() {
        let err = checked(|p| p.pub_inputs.public_in = 1).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_rejects_public_out_zero() {
        let err = checked(|p| p.pub_inputs.public_out = 0).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_rejects_recipient_mismatch() {
        let err = checked(|p| {
            p.pub_inputs.recipient = "0x000000000000000000000000000000000000dead".into()
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_rejects_payer_mismatch() {
        let err = checked(|p| {
            p.swap.deposit_d.payer = "0x000000000000000000000000000000000000dead".into()
        })
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_rejects_zero_deposit_public_in() {
        let err = checked(|p| p.swap.deposit_d.public_in = 0).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_rejects_leg1_chain_id_mismatch() {
        let err = checked(|p| p.pub_inputs.chain_id = CHAIN_ID as u64 + 1).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_rejects_leg2_chain_id_mismatch() {
        let err = checked(|p| p.swap.deposit_d.chain_id = CHAIN_ID as u64 + 1).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    /// The transact proof pins a relayer address; another relayer's
    /// submission cannot satisfy it, so it must not reach the prover.
    #[test]
    fn validate_rejects_a_proof_bound_to_another_relayer() {
        let err =
            checked(|p| p.pub_inputs.relayer = "0x000000000000000000000000000000000000dead".into())
                .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_rejects_duplicate_nullifiers() {
        let err =
            checked(|p| p.pub_inputs.nullifier[2] = p.pub_inputs.nullifier[0].clone()).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
