//! Builders mapping relayer DTOs and tree state into the on-chain `IMasp`
//! argument structs. Pure conversions, no I/O.

use crate::adapters::abi::IMasp;
use crate::adapters::parse::{parse_address, parse_b32, parse_u256};
use crate::domain::dto::{
    DepositRequestDto, OutputAuxDto, PointDto, ProofDto, PubInputsDto, TRANSACT_IN, TRANSACT_OUT,
};
use crate::domain::error::AppResult;
use alloy::primitives::{FixedBytes, U256};
use common_crypto::tree::Field;
use groth16::TreeUpdateBatchProof;

/// The batch shape lives in `domain::batch`; it is pure protocol arithmetic with
/// no ABI in it. Re-exported here so this module stays the one import a calldata
/// builder needs.
pub use crate::domain::batch::{
    DepositLeaf, LEAVES_PER_DEPOSIT, MAX_DEPOSITS_PER_BATCH, MAX_L_BATCH, PaddedBatch,
};

pub fn build_proof(p: &ProofDto) -> AppResult<IMasp::Proof> {
    Ok(IMasp::Proof {
        a: [parse_u256(&p.pi_a[0])?, parse_u256(&p.pi_a[1])?],
        b: [
            // snarkjs stores `pi_b` low-then-high while the on-chain Solidity
            // verifier expects [imag, real]. The swap matches SDK fixture
            // generation.
            [parse_u256(&p.pi_b[0][1])?, parse_u256(&p.pi_b[0][0])?],
            [parse_u256(&p.pi_b[1][1])?, parse_u256(&p.pi_b[1][0])?],
        ],
        c: [parse_u256(&p.pi_c[0])?, parse_u256(&p.pi_c[1])?],
    })
}

fn parse_point(p: &PointDto) -> AppResult<[U256; 2]> {
    Ok([parse_u256(&p.x)?, parse_u256(&p.y)?])
}

/// Parse a fixed-arity slice into an array, propagating the first error.
/// `try_map` on arrays is unstable and the shape is pinned by the DTO, so the
/// collect-then-unwrap cannot mis-size.
fn parse_points<const N: usize>(pts: &[PointDto; N]) -> AppResult<[[U256; 2]; N]> {
    let mut out = [[U256::ZERO; 2]; N];
    for (slot, p) in out.iter_mut().zip(pts.iter()) {
        *slot = parse_point(p)?;
    }
    Ok(out)
}

fn parse_b32s<const N: usize>(vals: &[String; N]) -> AppResult<[FixedBytes<32>; N]> {
    let mut out = [FixedBytes::<32>::ZERO; N];
    for (slot, v) in out.iter_mut().zip(vals.iter()) {
        *slot = parse_b32(v)?;
    }
    Ok(out)
}

pub fn build_pub_inputs(pi: &PubInputsDto) -> AppResult<IMasp::Transact> {
    Ok(IMasp::Transact {
        merkleRoot: parse_b32(&pi.merkle_root)?,
        nullifier: parse_b32s::<TRANSACT_IN>(&pi.nullifier)?,
        outCm: parse_b32s::<TRANSACT_OUT>(&pi.out_cm)?,
        publicAssetId: pi.public_asset_id,
        publicIn: pi.public_in,
        publicOut: pi.public_out,
        inCv: parse_points::<TRANSACT_IN>(&pi.in_cv)?,
        outCv: parse_points::<TRANSACT_OUT>(&pi.out_cv)?,
        outCvDep: parse_points::<TRANSACT_OUT>(&pi.out_cv_dep)?,
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

/// Build the `TreeUpdateBatch` public inputs for `flushBatch` or a spend.
pub fn build_tu_batch_pub_inputs(
    start_index: u64,
    old_root: &Field,
    new_root: &Field,
    batch: &PaddedBatch,
) -> IMasp::TreeUpdateBatch {
    IMasp::TreeUpdateBatch {
        oldRoot: FixedBytes::<32>::from(*old_root),
        newRoot: FixedBytes::<32>::from(*new_root),
        startIndex: start_index,
        actualCount: batch.actual_count,
        cms: batch.cms,
        cvDeps: batch.cv_deps,
        leafAsset: batch.leaf_asset,
        leafPublicIn: batch.leaf_public_in,
        isDeposit: batch.is_deposit,
    }
}

pub fn build_one_aux(a: &OutputAuxDto) -> AppResult<IMasp::OutputAux> {
    let bytes = hex::decode(a.ciphertext.trim_start_matches("0x"))
        .map_err(|e| crate::domain::error::AppError::BadRequest(format!("aux hex: {}", e)))?;
    Ok(IMasp::OutputAux {
        clueRx: parse_u256(&a.clue_r.x)?,
        clueRy: parse_u256(&a.clue_r.y)?,
        ephPubX: parse_u256(&a.eph_pub.x)?,
        ephPubY: parse_u256(&a.eph_pub.y)?,
        ciphertext: bytes.into(),
    })
}

/// One aux payload per transact output leaf.
pub fn build_aux(
    aux: &[OutputAuxDto; TRANSACT_OUT],
) -> AppResult<[IMasp::OutputAux; TRANSACT_OUT]> {
    let built: Vec<IMasp::OutputAux> = aux.iter().map(build_one_aux).collect::<AppResult<_>>()?;
    built
        .try_into()
        .map_err(|_| crate::domain::error::AppError::Internal("aux arity".into()))
}

/// Map a wire deposit request into the on-chain struct. Used by the swap
/// pipeline; the plain deposit path is wallet-driven and reaches the relayer only
/// through the flush flow.
pub fn build_deposit_request(d: &DepositRequestDto) -> AppResult<IMasp::DepositRequest> {
    Ok(IMasp::DepositRequest {
        chainId: U256::from(d.chain_id),
        publicAssetId: d.public_asset_id,
        publicIn: d.public_in,
        payer: parse_address(&d.payer)?,
        recipient: parse_address(&d.recipient)?,
        outCm: parse_b32(&d.out_cm)?,
        cvDep: [parse_u256(&d.cv_dep[0])?, parse_u256(&d.cv_dep[1])?],
        rcv: parse_u256(&d.rcv)?,
        feeIn: d.fee_in,
        feeCm: parse_b32(&d.fee_cm)?,
        feeCvDep: [parse_u256(&d.fee_cv_dep[0])?, parse_u256(&d.fee_cv_dep[1])?],
        feeRcv: parse_u256(&d.fee_rcv)?,
    })
}
