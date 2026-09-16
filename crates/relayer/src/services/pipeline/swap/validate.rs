//! Everything checked about a swap payload before it costs a Groth16: its shape,
//! its refund target, its deadline and the intent hash its proof carries.

use crate::adapters::abi::{IMasp, ISwapWrapper};
use crate::adapters::calldata::{
    build_aux, build_deposit_request, build_one_aux, build_proof, build_pub_inputs,
};
use crate::adapters::parse::{FieldRef, parse_address, parse_field, parse_hex_bytes, parse_u256};
use crate::domain::dto::{DepositRequestDto, SubmitSwapPayload};
use crate::domain::error::{AppError, AppResult};
use crate::domain::field::BN254_R;
use crate::services::pipeline::SUBMISSION_TIMEOUT;
use crate::services::pipeline::batcher;
use crate::services::pipeline::transact::TransactBinding;
use alloy::primitives::{Address, FixedBytes, U256, keccak256};
use alloy::sol_types::SolValue;
use std::time::{SystemTime, UNIX_EPOCH};

/// Why `refund_to` cannot own a swap's output escrow, if it cannot: the wrapper
/// reverts `InvalidRefundTo` on zero or itself, and the Bundler has no way to
/// move a refund out. Shared by request validation and the boot check on the
/// address this relayer advertises.
pub fn refund_address_error(
    refund_to: Address,
    wrapper: Option<Address>,
    bundler: Address,
) -> Option<String> {
    [
        (Some(Address::ZERO), "the zero address"),
        (Some(bundler), "this relayer's Bundler"),
        (wrapper, "the swap wrapper"),
    ]
    .into_iter()
    .find(|(forbidden, _)| *forbidden == Some(refund_to))
    .map(|(_, what)| format!("{refund_to} is {what}, which cannot hold a refund"))
}

/// `swap.refundTo`, refused here rather than after a Groth16.
pub(super) fn validate_refund_to(
    refund_to: Address,
    wrapper: Address,
    bundler: Address,
) -> AppResult<()> {
    match refund_address_error(refund_to, Some(wrapper), bundler) {
        Some(why) => Err(AppError::BadRequest(format!("swap.refundTo: {why}"))),
        None => Ok(()),
    }
}

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

/// How long before its deadline a swap is still accepted. A swap accepted
/// closer to it could be proved and bundled only after the deadline, and the
/// wrapper would then refund it rather than swap, with the wallet's fees paid.
/// The longest a caller waits for a submission, since one still queued past
/// that is already a slow bundle.
pub(super) const SWAP_DEADLINE_MARGIN_SECS: u64 = SUBMISSION_TIMEOUT.as_secs();

/// Parse every caller-supplied swap field into `SwapArgs`, leaving the tree
/// proof (`tp_w`, `tpi_w`) at its default for the batcher to fill.
pub(super) fn build_swap_args(payload: &SubmitSwapPayload) -> AppResult<ISwapWrapper::SwapArgs> {
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
        // Placeholders: the batcher fills the tree proof in at encode.
        tp_w: batcher::zero_proof(),
        tpi_w: IMasp::SpendTree {
            newRoot: FixedBytes::ZERO,
            startIndex: 0,
            anchorIndex: 0,
        },
        aux_w: build_aux(&payload.aux)?,
        deposit_d: build_deposit_request(&payload.swap.deposit_d)?,
        aux_d: build_one_aux(&payload.swap.aux_d)?,
        fee_aux_d: build_one_aux(&payload.swap.fee_aux_d)?,
        refund_d: build_deposit_request(&payload.swap.refund_d)?,
        refund_aux_d: build_one_aux(&payload.swap.refund_aux_d)?,
        refund_fee_aux_d: build_one_aux(&payload.swap.refund_fee_aux_d)?,
    })
}

/// The shape checks leg 2's deposit and the refund deposit share: each is
/// chain-bound on its own, paid for by the wrapper, and hashed into the tree by
/// the flush that materialises it.
fn check_swap_deposit(
    d: &DepositRequestDto,
    name: &'static str,
    binding: &TransactBinding,
    wrapper: Address,
) -> AppResult<()> {
    // It rides in the same calldata as leg 1, but the wrapper escrows it into
    // MASP under its own `chainId` field.
    if d.chain_id != binding.chain_id as u64 {
        return Err(AppError::BadRequest(format!(
            "{name}.chainId ({}) must equal the request chainId ({})",
            d.chain_id, binding.chain_id
        )));
    }
    let payer = parse_address(&d.payer)?;
    if payer != wrapper {
        return Err(AppError::BadRequest(format!(
            "{name}.payer ({payer}) must equal swap_wrapper_address ({wrapper})"
        )));
    }
    if d.public_in == 0 {
        return Err(AppError::BadRequest(format!("{name}.publicIn must be > 0")));
    }
    // Its leaf is hashed into the tree by the flush that materialises it, so its
    // field elements must be canonical for the same reason leg 1's are.
    let field = |suffix: &str| format!("{name}.{suffix}");
    parse_field(&d.out_cm, FieldRef::Named(&field("outCm")))?;
    for (i, v) in d.cv_dep.iter().enumerate() {
        parse_field(v, FieldRef::Index(&field("cvDep"), i))?;
    }
    parse_field(&d.rcv, FieldRef::Named(&field("rcv")))?;
    Ok(())
}

pub(super) fn validate_swap_shape(
    p: &SubmitSwapPayload,
    wrapper: Address,
    binding: TransactBinding,
) -> AppResult<ISwapWrapper::SwapArgs> {
    binding.check(&p.pub_inputs)?;
    // Leg 1 is structurally a withdraw: shielded notes to a public token held by
    // the wrapper. The transact SNARK enforces conservation, so these checks only
    // reject clearly wrong shapes early.
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
    check_swap_deposit(&p.swap.deposit_d, "deposit_d", &binding, wrapper)?;
    check_swap_deposit(&p.swap.refund_d, "refund_d", &binding, wrapper)?;
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if args.deadline < U256::from(now + SWAP_DEADLINE_MARGIN_SECS) {
        return Err(AppError::BadRequest(format!(
            "swap.deadline ({}) is less than {SWAP_DEADLINE_MARGIN_SECS}s away; the swap \
             could land after it and be refunded instead",
            args.deadline
        )));
    }
    check_intent(&args)?;
    Ok(args)
}
