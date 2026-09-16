//! Spend payload validation, against hand-built payloads.

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
            intent_hash: "0".into(),
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

/// `intentHash` binds a swap's terms and only `SwapWrapper` reads it, so a
/// spend validates whatever it carries, including the full 256-bit range.
#[test]
fn spends_ignore_the_intent_hash() {
    for (kind, public_out) in [
        (SpendKind::Transfer, 0),
        (SpendKind::Withdraw, 1_000),
        (SpendKind::WithdrawNative, 1_000),
    ] {
        for hash in ["0", "12345", &U256::MAX.to_string()] {
            checked(kind, public_out, |p| {
                p.pub_inputs.intent_hash = hash.to_string()
            })
            .unwrap_or_else(|e| panic!("{kind:?} with intentHash {hash}: {e}"));
        }
    }
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

/// The pipeline is selected by the envelope's chain id while the SNARK is bound
/// to the one inside `pub_inputs`, so a mismatch always reverts.
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

/// The adapter drives `MASP.withdraw` itself, so the proof must name it as
/// relayer; this relayer's own signer would revert `AdapterNotRelayer`.
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
