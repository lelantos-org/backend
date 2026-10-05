//! Everything checked about a swap payload before it costs a Groth16: its shape,
//! its deadline and the intent hash its proof carries.

use crate::adapters::abi::ISwapWrapper;
use crate::adapters::calldata::{
    build_aux, build_deposit_request, build_one_aux, build_proof, build_pub_inputs,
};
use crate::adapters::parse::{parse_address, parse_hex_bytes, parse_u256};
use crate::domain::dto::SubmitSwapPayload;
use crate::domain::error::{AppError, AppResult};
use crate::domain::field::BN254_R;
use crate::services::pipeline::wrapper::{WrapperBinding, check_deadline, placeholder_tree_update};
use alloy::primitives::{U256, keccak256};
use alloy::sol_types::SolValue;

/// `SwapWrapper._intentHash`: the value a swap's withdraw proof must
/// carry as `pi_w.intentHash`.
///
/// `keccak256(abi.encode(refundTo, tokenOut, minOut, adapter, deadline,
/// deposit_d, aux_d, fee_aux_d, refund_d, refund_aux_d, refund_fee_aux_d)) % R`.
/// Solidity's `abi.encode` of a parameter list is `abi_encode_params`;
/// `abi_encode` would treat the tuple as one dynamic value and prepend an offset
/// word.
pub(super) fn swap_intent_hash(a: &ISwapWrapper::SwapArgs) -> U256 {
    let encoded = (
        a.refundTo,
        a.tokenOut,
        a.minOut,
        a.adapter,
        a.deadline,
        a.deposit_d.clone(),
        a.aux_d.clone(),
        a.fee_aux_d.clone(),
        a.refund_d.clone(),
        a.refund_aux_d.clone(),
        a.refund_fee_aux_d.clone(),
    )
        .abi_encode_params();
    U256::from_be_bytes(keccak256(encoded).0) % *BN254_R
}

/// The wrapper reverts `IntentMismatch` unless the proof's `intentHash` covers
/// exactly these output terms. The contract is the guard; checking here only
/// spares a Groth16 and a bundle slot on a payload that would revert.
fn check_intent(a: &ISwapWrapper::SwapArgs) -> AppResult<()> {
    let expected = swap_intent_hash(a);
    if a.pi_w.intentHash != expected {
        return Err(AppError::BadRequest(format!(
            "pubInputs.intentHash ({}) does not match the swap's refundTo, tokenOut, \
             minOut, adapter, deadline, depositD, auxD, feeAuxD, refundD, refundAuxD \
             and refundFeeAuxD ({expected})",
            a.pi_w.intentHash
        )));
    }
    Ok(())
}

/// Parse every caller-supplied swap field into `SwapArgs`, leaving the tree
/// proof (`tp_w`, `tpi_w`) at its default for the batcher to fill.
pub(super) fn build_swap_args(payload: &SubmitSwapPayload) -> AppResult<ISwapWrapper::SwapArgs> {
    let (tp_w, tpi_w) = placeholder_tree_update();
    Ok(ISwapWrapper::SwapArgs {
        tokenIn: parse_address(&payload.swap.token_in)?,
        tokenOut: parse_address(&payload.swap.token_out)?,
        amountIn: parse_u256(&payload.swap.amount_in)?,
        minOut: parse_u256(&payload.swap.min_out)?,
        adapter: parse_address(&payload.swap.adapter)?,
        route: parse_hex_bytes(&payload.swap.route, "route")?,
        deadline: parse_u256(&payload.swap.deadline)?,
        refundTo: parse_address(&payload.swap.refund_to)?,
        p_w: build_proof(&payload.proof)?,
        pi_w: build_pub_inputs(&payload.pub_inputs)?,
        tp_w,
        tpi_w,
        aux_w: build_aux(&payload.aux)?,
        deposit_d: build_deposit_request(&payload.swap.deposit_d)?,
        aux_d: build_one_aux(&payload.swap.aux_d)?,
        fee_aux_d: build_one_aux(&payload.swap.fee_aux_d)?,
        refund_d: build_deposit_request(&payload.swap.refund_d)?,
        refund_aux_d: build_one_aux(&payload.swap.refund_aux_d)?,
        refund_fee_aux_d: build_one_aux(&payload.swap.refund_fee_aux_d)?,
    })
}

pub(super) fn validate_swap_shape(
    p: &SubmitSwapPayload,
    binding: &WrapperBinding,
) -> AppResult<ISwapWrapper::SwapArgs> {
    binding.check_leg1(&p.pub_inputs)?;
    binding.check_deposit(&p.swap.deposit_d, "deposit_d")?;
    binding.check_deposit(&p.swap.refund_d, "refund_d")?;
    // `minOut == 0` accepts any output, which is full sandwich exposure. The
    // wrapper honours it; the relayer does not relay it.
    if parse_u256(&p.swap.min_out)?.is_zero() {
        return Err(AppError::BadRequest(
            "swap.minOut must be > 0; a zero floor accepts any output".into(),
        ));
    }
    // Parse every caller-supplied field the calldata encoder uses, here, before
    // anything expensive or stateful runs. The batcher encodes the call after
    // reserving the whole bundle's leaves, so a swap naming an unparseable
    // `adapter` would cost every operation in it a retry. The parsed args are
    // kept, so the encoder does not parse again.
    let args = build_swap_args(p)?;
    check_deadline(args.deadline, "swap.deadline")?;
    check_intent(&args)?;
    Ok(args)
}
