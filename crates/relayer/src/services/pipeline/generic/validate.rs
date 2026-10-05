//! Everything checked about a generic payload before it costs a Groth16: its
//! shape, its receivers, its deadline, its gas floor, its calls and the intent
//! hash its proof carries.

use super::allowlist::{AllowedCalls, check_calls};
use crate::adapters::abi::IGenericCallWrapper;
use crate::adapters::calldata::{
    build_aux, build_deposit_request, build_one_aux, build_proof, build_pub_inputs,
};
use crate::adapters::parse::{parse_address, parse_hex_bytes, parse_u256};
use crate::domain::dto::{GenericBlob, GenericCallDto, GenericOutputDto, SubmitGenericPayload};
use crate::domain::error::{AppError, AppResult};
use crate::domain::field::BN254_R;
use crate::services::pipeline::wrapper::{WrapperBinding, check_deadline, placeholder_tree_update};
use alloy::primitives::{U256, keccak256};
use alloy::sol_types::SolValue;

/// `GenericCallWrapper.MAX_OUTPUTS`.
const MAX_OUTPUTS: usize = 4;
/// `GenericCallWrapper.MAX_CALLS`.
const MAX_CALLS: usize = 16;

/// What one chain's generic payloads are checked against.
pub struct GenericPolicy {
    /// The chain's `GenericCallWrapper` and this relayer's `Bundler`.
    pub binding: WrapperBinding,
    pub allowed_calls: AllowedCalls,
    /// Largest `generic.minGas` accepted.
    pub max_min_gas: u64,
}

/// `GenericCallWrapper.intentHash`: the value a generic call's withdraw proof
/// must carry as `pi_w.intentHash`.
///
/// `keccak256(abi.encode(refundTo, surplusTo, deadline, minGas, calls, outputs,
/// refund_d, refund_aux_d, refund_fee_aux_d)) % R`. See `swap_intent_hash` for
/// `abi_encode_params`.
pub(super) fn generic_intent_hash(a: &IGenericCallWrapper::GenericArgs) -> U256 {
    let encoded = (
        a.refundTo,
        a.surplusTo,
        a.deadline,
        a.minGas,
        a.calls.clone(),
        a.outputs.clone(),
        a.refund_d.clone(),
        a.refund_aux_d.clone(),
        a.refund_fee_aux_d.clone(),
    )
        .abi_encode_params();
    U256::from_be_bytes(keccak256(encoded).0) % *BN254_R
}

/// The wrapper reverts `IntentMismatch` unless the proof's `intentHash` covers
/// exactly these terms. The contract is the guard; checking here only spares a
/// Groth16 and a bundle slot on a payload that would revert.
fn check_intent(a: &IGenericCallWrapper::GenericArgs) -> AppResult<()> {
    let expected = generic_intent_hash(a);
    if a.pi_w.intentHash != expected {
        return Err(AppError::BadRequest(format!(
            "pubInputs.intentHash ({}) does not match the payload's refundTo, surplusTo, \
             deadline, minGas, calls, outputs, refundD, refundAuxD and refundFeeAuxD \
             ({expected})",
            a.pi_w.intentHash
        )));
    }
    Ok(())
}

/// `min_gas` as the gas units a submission is charged for its call leg, refused
/// when zero or above `max`.
pub(super) fn check_min_gas(min_gas: U256, max: u64) -> AppResult<u64> {
    if min_gas.is_zero() || min_gas > U256::from(max) {
        return Err(AppError::BadRequest(format!(
            "generic.minGas ({min_gas}) must be between 1 and {max}"
        )));
    }
    Ok(min_gas.to::<u64>())
}

/// `MAX_OUTPUTS`, `MAX_CALLS` and the shape of every deposit the wrapper
/// escrows.
fn check_shape(g: &GenericBlob, binding: &WrapperBinding) -> AppResult<()> {
    if g.outputs.is_empty() || g.outputs.len() > MAX_OUTPUTS {
        return Err(AppError::BadRequest(format!(
            "generic.outputs must hold between 1 and {MAX_OUTPUTS} outputs, got {}",
            g.outputs.len()
        )));
    }
    if g.calls.len() > MAX_CALLS {
        return Err(AppError::BadRequest(format!(
            "generic.calls must hold at most {MAX_CALLS} calls, got {}",
            g.calls.len()
        )));
    }
    for (i, o) in g.outputs.iter().enumerate() {
        binding.check_deposit(&o.deposit, &format!("generic.outputs[{i}].deposit"))?;
    }
    binding.check_deposit(&g.refund_d, "generic.refundD")
}

/// The wrapper reverts `AmountInZero` and `MinOutZero` on these.
fn check_amounts(a: &IGenericCallWrapper::GenericArgs) -> AppResult<()> {
    if a.amountIn.is_zero() {
        return Err(AppError::BadRequest("generic.amountIn must be > 0".into()));
    }
    for (i, o) in a.outputs.iter().enumerate() {
        if o.minOut.is_zero() {
            return Err(AppError::BadRequest(format!(
                "generic.outputs[{i}].minOut must be > 0; a zero floor accepts any output"
            )));
        }
    }
    Ok(())
}

fn build_call(c: &GenericCallDto) -> AppResult<IGenericCallWrapper::Call> {
    Ok(IGenericCallWrapper::Call {
        target: parse_address(&c.target)?,
        value: parse_u256(&c.value)?,
        data: parse_hex_bytes(&c.data, "call data")?,
    })
}

fn build_output(o: &GenericOutputDto) -> AppResult<IGenericCallWrapper::Output> {
    Ok(IGenericCallWrapper::Output {
        minOut: parse_u256(&o.min_out)?,
        deposit: build_deposit_request(&o.deposit)?,
        aux: build_one_aux(&o.aux)?,
        feeAux: build_one_aux(&o.fee_aux)?,
    })
}

/// Parse every caller-supplied field into `GenericArgs`, leaving the tree proof
/// (`tp_w`, `tpi_w`) at its default for the batcher to fill.
pub(super) fn build_generic_args(
    payload: &SubmitGenericPayload,
) -> AppResult<IGenericCallWrapper::GenericArgs> {
    let g = &payload.generic;
    let (tp_w, tpi_w) = placeholder_tree_update();
    Ok(IGenericCallWrapper::GenericArgs {
        amountIn: parse_u256(&g.amount_in)?,
        calls: g.calls.iter().map(build_call).collect::<AppResult<_>>()?,
        outputs: g
            .outputs
            .iter()
            .map(build_output)
            .collect::<AppResult<_>>()?,
        deadline: parse_u256(&g.deadline)?,
        minGas: parse_u256(&g.min_gas)?,
        refundTo: parse_address(&g.refund_to)?,
        surplusTo: parse_address(&g.surplus_to)?,
        p_w: build_proof(&payload.proof)?,
        pi_w: build_pub_inputs(&payload.pub_inputs)?,
        tp_w,
        tpi_w,
        aux_w: build_aux(&payload.aux)?,
        refund_d: build_deposit_request(&g.refund_d)?,
        refund_aux_d: build_one_aux(&g.refund_aux_d)?,
        refund_fee_aux_d: build_one_aux(&g.refund_fee_aux_d)?,
    })
}

/// Check `p` against `policy`. Returns the parsed `GenericArgs`, minus the tree
/// proof only the batcher can fill, and `minGas` as gas units.
pub(super) fn validate_generic(
    p: &SubmitGenericPayload,
    policy: &GenericPolicy,
) -> AppResult<(IGenericCallWrapper::GenericArgs, u64)> {
    let binding = &policy.binding;
    binding.check_leg1(&p.pub_inputs)?;
    binding.check_payer(&p.pub_inputs)?;
    check_shape(&p.generic, binding)?;

    // Every caller-supplied field the calldata encoder uses is parsed here, and
    // the parsed args kept: the batcher encodes after reserving the whole
    // bundle's leaves, where a failure costs every operation in it a retry.
    let args = build_generic_args(p)?;
    check_amounts(&args)?;
    binding.check_receiver(args.refundTo, "generic.refundTo")?;
    binding.check_receiver(args.surplusTo, "generic.surplusTo")?;
    check_deadline(args.deadline, "generic.deadline")?;
    let min_gas = check_min_gas(args.minGas, policy.max_min_gas)?;
    check_calls(&args.calls, &policy.allowed_calls)?;
    // Checked last, so a malformed field reports its own error.
    check_intent(&args)?;
    Ok((args, min_gas))
}
