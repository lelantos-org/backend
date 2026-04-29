// Builders that map relayer DTOs + tree state into the on-chain `IMasp`
// argument structs. Pure conversions; no I/O.

use crate::adapters::abi::IMasp;
use crate::adapters::parse::{parse_address, parse_b32, parse_u256};
use crate::domain::dto::{DepositIntentDto, OutputAuxDto, ProofDto, PubInputsDto};
use crate::domain::error::AppResult;
use crate::services::prover::TreeUpdateBatchProof;
use alloy::primitives::{FixedBytes, U256};
use fmd_crypto::tree::Field;

/// Max batch size (mirrors `PubInputs.MAX_N_BATCH` in the contract).
pub const MAX_N_BATCH: usize = 8;

pub fn build_proof(p: &ProofDto) -> AppResult<IMasp::Proof> {
    Ok(IMasp::Proof {
        a: [parse_u256(&p.pi_a[0])?, parse_u256(&p.pi_a[1])?],
        b: [
            // snarkjs proof: pi_b stored low-then-high; on-chain Solidity
            // verifier expects [imag, real]. Swap matches SDK fixture-gen.
            [parse_u256(&p.pi_b[0][1])?, parse_u256(&p.pi_b[0][0])?],
            [parse_u256(&p.pi_b[1][1])?, parse_u256(&p.pi_b[1][0])?],
        ],
        c: [parse_u256(&p.pi_c[0])?, parse_u256(&p.pi_c[1])?],
    })
}

pub fn build_pub_inputs(pi: &PubInputsDto) -> AppResult<IMasp::Transact> {
    Ok(IMasp::Transact {
        merkleRoot: parse_b32(&pi.merkle_root)?,
        nullifier: [parse_b32(&pi.nullifier[0])?, parse_b32(&pi.nullifier[1])?],
        outCm: [parse_b32(&pi.out_cm[0])?, parse_b32(&pi.out_cm[1])?],
        publicAssetId: pi.public_asset_id,
        publicIn: pi.public_in,
        publicOut: pi.public_out,
        inCv: [
            [parse_u256(&pi.in_cv[0].x)?, parse_u256(&pi.in_cv[0].y)?],
            [parse_u256(&pi.in_cv[1].x)?, parse_u256(&pi.in_cv[1].y)?],
        ],
        outCv: [
            [parse_u256(&pi.out_cv[0].x)?, parse_u256(&pi.out_cv[0].y)?],
            [parse_u256(&pi.out_cv[1].x)?, parse_u256(&pi.out_cv[1].y)?],
        ],
        outCvDep: [
            [
                parse_u256(&pi.out_cv_dep[0].x)?,
                parse_u256(&pi.out_cv_dep[0].y)?,
            ],
            [
                parse_u256(&pi.out_cv_dep[1].x)?,
                parse_u256(&pi.out_cv_dep[1].y)?,
            ],
        ],
        recipient: parse_address(&pi.recipient)?,
        chainId: U256::from(pi.chain_id),
        payer: parse_address(&pi.payer)?,
        relayer: parse_address(&pi.relayer)?,
    })
}

pub fn build_tu_proof(tp: &TreeUpdateBatchProof) -> AppResult<IMasp::Proof> {
    Ok(IMasp::Proof {
        a: [parse_u256(&tp.pi_a[0])?, parse_u256(&tp.pi_a[1])?],
        b: [
            [parse_u256(&tp.pi_b[0][1])?, parse_u256(&tp.pi_b[0][0])?],
            [parse_u256(&tp.pi_b[1][1])?, parse_u256(&tp.pi_b[1][0])?],
        ],
        c: [parse_u256(&tp.pi_c[0])?, parse_u256(&tp.pi_c[1])?],
    })
}

/// Build `TreeUpdateBatch` PI for `flushBatch` or spend ops. Padding
/// entries (i ≥ 2 * actual_count for the cm / cvDep arrays, and
/// i ≥ actual_count for the per-pair arrays) MUST be zero — caller is
/// responsible for padding. The circuit + contract jointly enforce this.
#[allow(clippy::too_many_arguments)]
pub fn build_tu_batch_pub_inputs(
    start_index: u64,
    old_root: &Field,
    new_root: &Field,
    cms: [FixedBytes<32>; 2 * MAX_N_BATCH],
    cv_deps: [[U256; 2]; 2 * MAX_N_BATCH],
    pair_asset: [u64; MAX_N_BATCH],
    pair_public_in: [u64; MAX_N_BATCH],
    is_deposit: [u8; MAX_N_BATCH],
    actual_count: u64,
) -> IMasp::TreeUpdateBatch {
    IMasp::TreeUpdateBatch {
        oldRoot: FixedBytes::<32>::from(*old_root),
        newRoot: FixedBytes::<32>::from(*new_root),
        startIndex: start_index,
        actualCount: actual_count,
        cms,
        cvDeps: cv_deps,
        pairAsset: pair_asset,
        pairPublicIn: pair_public_in,
        isDeposit: is_deposit,
    }
}

pub fn build_aux(aux: &[OutputAuxDto; 2]) -> AppResult<[IMasp::OutputAux; 2]> {
    let make = |a: &OutputAuxDto| -> AppResult<IMasp::OutputAux> {
        let bytes = hex::decode(a.ciphertext.trim_start_matches("0x"))
            .map_err(|e| crate::domain::error::AppError::BadRequest(format!("aux hex: {}", e)))?;
        Ok(IMasp::OutputAux {
            clueRx: parse_u256(&a.clue_r.x)?,
            clueRy: parse_u256(&a.clue_r.y)?,
            ephPubX: parse_u256(&a.eph_pub.x)?,
            ephPubY: parse_u256(&a.eph_pub.y)?,
            ciphertext: bytes.into(),
        })
    };
    Ok([make(&aux[0])?, make(&aux[1])?])
}

/// Map a wire `DepositIntent` into the on-chain struct. Used by the swap
/// pipeline; the legacy spend path takes the intent server-side via the
/// flush flow instead, so this lives here rather than next to the spend
/// builders.
pub fn build_deposit_intent(d: &DepositIntentDto) -> AppResult<IMasp::DepositIntent> {
    Ok(IMasp::DepositIntent {
        chainId: d.chain_id,
        publicAssetId: d.public_asset_id,
        publicIn: d.public_in,
        payer: parse_address(&d.payer)?,
        recipient: parse_address(&d.recipient)?,
        outCm: [parse_b32(&d.out_cm[0])?, parse_b32(&d.out_cm[1])?],
        cvDep0: [parse_u256(&d.cv_dep0[0])?, parse_u256(&d.cv_dep0[1])?],
        cvDep1: [parse_u256(&d.cv_dep1[0])?, parse_u256(&d.cv_dep1[1])?],
        rcvTotal: parse_u256(&d.rcv_total)?,
    })
}
