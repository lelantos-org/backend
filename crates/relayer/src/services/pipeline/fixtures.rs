//! Hand-built payload parts the wrapper pipelines' tests share.

use crate::domain::dto::{DepositRequestDto, OutputAuxDto, PointDto, ProofDto, PubInputsDto};
use crate::services::pipeline::wrapper::WrapperBinding;
use alloy::primitives::Address;
use std::array;

pub(super) const CHAIN_ID: i64 = 31337;

pub(super) fn wrapper() -> Address {
    Address::repeat_byte(0x77)
}

pub(super) fn bundler() -> Address {
    Address::repeat_byte(0x88)
}

pub(super) fn binding() -> WrapperBinding {
    WrapperBinding {
        chain_id: CHAIN_ID,
        wrapper: wrapper(),
        bundler: bundler(),
    }
}

pub(super) fn fake_proof() -> ProofDto {
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

pub(super) fn one_aux() -> OutputAuxDto {
    let p = PointDto {
        x: "0".into(),
        y: "0".into(),
    };
    OutputAuxDto {
        clue_r: p.clone(),
        clue_q: p.clone(),
        eph_pub: p,
        ciphertext: "0x".into(),
    }
}

/// Leg 1 of a withdraw of `public_out` to `recipient`. `payer`, `relayer` and
/// `intent_hash` are left for the caller to bind.
pub(super) fn fake_pub_inputs(recipient: &str, public_out: u64) -> PubInputsDto {
    PubInputsDto {
        merkle_root: format!("0x{:0>64}", "0"),
        nullifier: array::from_fn(|i| format!("0x{:0>64}", i + 1)),
        out_cm: array::from_fn(|i| format!("0x{:0>64}", i + 10)),
        public_asset_id: 1,
        public_out,
        digest: "0".into(),
        recipient: recipient.to_string(),
        chain_id: CHAIN_ID as u64,
        payer: "0x0000000000000000000000000000000000000000".into(),
        relayer: "0x0000000000000000000000000000000000000000".into(),
        intent_hash: "0".into(),
    }
}

pub(super) fn fake_deposit(payer: &str, public_in: u64) -> DepositRequestDto {
    DepositRequestDto {
        chain_id: CHAIN_ID as u64,
        public_asset_id: 2,
        public_in,
        payer: payer.to_string(),
        recipient: "0x000000000000000000000000000000000000beef".into(),
        inner: format!("0x{:0>64}", "5"),
        // The relayer is paid on the withdraw leg, so the deposit's fee leaf is
        // a zero-value pad, which names no fee asset.
        fee_asset_id: 0,
        fee_in: 0,
        fee_inner: format!("0x{:0>64}", "6"),
    }
}
