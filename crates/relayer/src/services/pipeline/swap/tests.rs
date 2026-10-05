//! Swap payload validation and the intent hash, against hand-built payloads.

use super::validate::{build_swap_args, swap_intent_hash, validate_swap_shape};
use super::*;
use crate::domain::dto::{DepositRequestDto, SwapBlob};
use crate::domain::error::AppError;
use crate::services::pipeline::fixtures::{
    CHAIN_ID, binding, fake_deposit, fake_proof, fake_pub_inputs, one_aux, wrapper,
};
use crate::services::pipeline::wrapper::DEADLINE_MARGIN_SECS;
use alloy::primitives::{FixedBytes, U256};
use std::array;
use std::time::{SystemTime, UNIX_EPOCH};

/// A swap whose proof commits to its own terms, as an honest wallet builds it.
fn fake_payload(wrapper: &str) -> SubmitSwapPayload {
    let mut p = SubmitSwapPayload {
        chain_id: CHAIN_ID,
        proof: fake_proof(),
        pub_inputs: fake_pub_inputs(wrapper, 1_000),
        aux: array::from_fn(|_| one_aux()),
        swap: SwapBlob {
            adapter: "0x0000000000000000000000000000000000000001".into(),
            // 64-byte single hop:
            // `abi.encode(uint24 fee, uint160 sqrtPriceLimitX96)`.
            route:
                "0x00000000000000000000000000000000000000000000000000000000000000640000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
            deposit_d: fake_deposit(wrapper, 990),
            aux_d: one_aux(),
            fee_aux_d: one_aux(),
            refund_d: DepositRequestDto {
                public_asset_id: 1,
                inner: format!("0x{:0>64}", "7"),
                ..fake_deposit(wrapper, 995)
            },
            refund_aux_d: one_aux(),
            refund_fee_aux_d: one_aux(),
            token_in: "0x0000000000000000000000000000000000000111".into(),
            token_out: "0x0000000000000000000000000000000000000222".into(),
            amount_in: "1000".into(),
            min_out: "990".into(),
            deadline: "1900000000".into(),
            refund_to: "0x000000000000000000000000000000000000cafe".into(),
        },
    };
    p.pub_inputs.intent_hash = swap_intent_hash(&build_swap_args(&p).unwrap()).to_string();
    p
}

/// Payload as a well-behaved wallet would send it, plus a mutation.
///
/// Leg 1 is bound to the wrapper throughout: the wrapper calls `MASP.withdraw`,
/// so it is the pool's `msg.sender` and the address `pi.relayer` must name.
/// Binding these to the relayer's own signer would 400 every real swap.
fn checked(mutate: impl FnOnce(&mut SubmitSwapPayload)) -> AppResult<()> {
    let w = wrapper().to_string();
    let mut p = fake_payload(&w);
    p.pub_inputs.relayer = w;
    mutate(&mut p);
    validate_swap_shape(&p, &binding()).map(|_| ())
}

#[test]
fn validate_accepts_well_formed_payload() {
    checked(|_| {}).unwrap();
}

/// The flush that materialises the escrow reverts `CoefficientOutOfField` on
/// either word, so the swap would leave a deposit nobody can flush.
#[test]
fn validate_rejects_a_deposit_inner_that_is_not_a_field_element() {
    let modulus = crate::domain::field::BN254_R.to_string();
    type Mutation = fn(&mut SubmitSwapPayload, String);
    let cases: [(&str, Mutation); 4] = [
        ("deposit_d.inner", |p, v| p.swap.deposit_d.inner = v),
        ("deposit_d.feeInner", |p, v| p.swap.deposit_d.fee_inner = v),
        ("refund_d.inner", |p, v| p.swap.refund_d.inner = v),
        ("refund_d.feeInner", |p, v| p.swap.refund_d.fee_inner = v),
    ];
    for (field, mutate) in cases {
        let err = checked(|p| mutate(p, modulus.clone())).unwrap_err();
        assert!(
            matches!(&err, AppError::BadRequest(m) if m.contains(field)),
            "{field}: {err}"
        );
    }
}

#[test]
fn validate_rejects_public_out_zero() {
    let err = checked(|p| p.pub_inputs.public_out = 0).unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[test]
fn validate_rejects_recipient_mismatch() {
    let err =
        checked(|p| p.pub_inputs.recipient = "0x000000000000000000000000000000000000dead".into())
            .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[test]
fn validate_rejects_payer_mismatch() {
    let err =
        checked(|p| p.swap.deposit_d.payer = "0x000000000000000000000000000000000000dead".into())
            .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

/// `SwapWrapperBindingTest.INTENT_VECTOR`: pinned in `contracts/test` against
/// `SwapWrapper._intentHash` and in the SDK against `swapIntentHash`, so the
/// three encodings cannot drift apart unnoticed.
#[test]
fn intent_hash_matches_the_solidity_vector() {
    let a = |n: u64| Address::left_padding_from(&n.to_be_bytes());
    let u = U256::from;
    let aux = |base: u64, q: u64, ct: &[u8]| IMasp::OutputAux {
        clueRx: u(base),
        clueRy: u(base + 1),
        clueQx: u(q),
        clueQy: u(q + 1),
        ephPubX: u(base + 2),
        ephPubY: u(base + 3),
        ciphertext: ct.to_vec().into(),
    };
    let b32 = |n: u64| FixedBytes::<32>::from(u(n).to_be_bytes::<32>());
    // Only the hashed terms matter; the rest come from any valid payload.
    let args = ISwapWrapper::SwapArgs {
        refundTo: a(0x4EF0),
        tokenOut: a(0xB0B0),
        minOut: u(9_900_000_000_000u64),
        adapter: a(0xADA7),
        deadline: u(1_900_000_000u64),
        deposit_d: IMasp::DepositRequest {
            chainId: u(31337u64),
            publicAssetId: 2,
            publicIn: 990,
            payer: a(0x5A5A),
            recipient: a(0xBEEF),
            inner: b32(1),
            feeAssetId: 2,
            feeIn: 5,
            feeInner: b32(6),
        },
        aux_d: aux(10, 40, &[0x01, 0x02]),
        fee_aux_d: aux(14, 42, &[0x03, 0x04, 0x05]),
        refund_d: IMasp::DepositRequest {
            chainId: u(31337u64),
            publicAssetId: 1,
            publicIn: 995,
            payer: a(0x5A5A),
            recipient: a(0xBEEF),
            inner: b32(0x12),
            feeAssetId: 1,
            feeIn: 22,
            feeInner: b32(0x17),
        },
        refund_aux_d: aux(27, 44, &[0x06]),
        refund_fee_aux_d: aux(31, 46, &[0x07, 0x08]),
        ..build_swap_args(&fake_payload(&wrapper().to_string())).unwrap()
    };
    assert_eq!(
        swap_intent_hash(&args).to_string(),
        "20568246496086653981650821090611381092259931261982909181487420188972474156030"
    );
}

#[test]
fn validate_rejects_a_refund_the_wrapper_does_not_pay_for() {
    let err =
        checked(|p| p.swap.refund_d.payer = "0x000000000000000000000000000000000000dead".into())
            .unwrap_err();
    assert!(
        matches!(&err, AppError::BadRequest(m) if m.contains("refund_d.payer")),
        "{err}"
    );
}

#[test]
fn validate_rejects_a_refund_on_another_chain() {
    let err = checked(|p| p.swap.refund_d.chain_id = CHAIN_ID as u64 + 1).unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[test]
fn validate_rejects_an_intent_hash_the_terms_do_not_produce() {
    let err = checked(|p| p.pub_inputs.intent_hash = "1".into()).unwrap_err();
    assert!(
        matches!(&err, AppError::BadRequest(m) if m.contains("intentHash")),
        "{err}"
    );
}

/// An expired swap is refused before any proving, rather than refunded with
/// its fees paid.
#[test]
fn validate_rejects_an_expired_deadline() {
    let err = checked(|p| p.swap.deadline = "1".into()).unwrap_err();
    assert!(
        matches!(&err, AppError::BadRequest(m) if m.contains("swap.deadline")),
        "{err}"
    );
}

/// So is one too close to expiry to land before it.
#[test]
fn validate_rejects_a_deadline_inside_the_margin() {
    let soon = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + DEADLINE_MARGIN_SECS / 2;
    let err = checked(|p| {
        p.swap.deadline = soon.to_string();
        p.pub_inputs.intent_hash = swap_intent_hash(&build_swap_args(p).unwrap()).to_string();
    })
    .unwrap_err();
    assert!(
        matches!(&err, AppError::BadRequest(m) if m.contains("swap.deadline")),
        "{err}"
    );
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

/// The transact proof pins the caller, so a proof naming anything other than
/// the wrapper cannot satisfy `pi.relayer == msg.sender` and must not reach the
/// prover.
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
