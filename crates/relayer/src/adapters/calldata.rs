//! Builders mapping relayer DTOs and tree state into the on-chain `IMasp`
//! argument structs. Pure conversions, no I/O.

use crate::adapters::abi::IMasp;
use crate::adapters::parse::{parse_address, parse_b32, parse_hex_bytes, parse_u256};
use crate::domain::batch::PaddedBatch;
use crate::domain::dto::{
    DepositRequestDto, OutputAuxDto, ProofDto, PubInputsDto, TRANSACT_IN, TRANSACT_OUT,
};
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{FixedBytes, U256};
use crypto::tree::Field;
use groth16::TreeUpdateBatchProof;

/// A wallet's transact proof.
pub fn build_proof(p: &ProofDto) -> AppResult<IMasp::Proof> {
    snarkjs_proof(&p.pi_a, &p.pi_b, &p.pi_c)
}

/// A proof this relayer's prover made.
pub fn build_tu_proof(tp: &TreeUpdateBatchProof) -> AppResult<IMasp::Proof> {
    snarkjs_proof(&tp.pi_a, &tp.pi_b, &tp.pi_c)
}

/// A snarkjs-shaped proof, projective coordinates and all, as the on-chain
/// verifier takes it.
fn snarkjs_proof(
    pi_a: &[String; 3],
    pi_b: &[[String; 2]; 3],
    pi_c: &[String; 3],
) -> AppResult<IMasp::Proof> {
    Ok(IMasp::Proof {
        a: [parse_u256(&pi_a[0])?, parse_u256(&pi_a[1])?],
        b: [
            // snarkjs stores `pi_b` low-then-high while the on-chain Solidity
            // verifier expects [imag, real]. The swap matches SDK fixture
            // generation.
            [parse_u256(&pi_b[0][1])?, parse_u256(&pi_b[0][0])?],
            [parse_u256(&pi_b[1][1])?, parse_u256(&pi_b[1][0])?],
        ],
        c: [parse_u256(&pi_c[0])?, parse_u256(&pi_c[1])?],
    })
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
        publicOut: pi.public_out,
        digest: parse_u256(&pi.digest)?,
        recipient: parse_address(&pi.recipient)?,
        chainId: U256::from(pi.chain_id),
        payer: parse_address(&pi.payer)?,
        relayer: parse_address(&pi.relayer)?,
        intentHash: parse_u256(&pi.intent_hash)?,
    })
}

/// Build the `TreeUpdateBatch` public inputs for `flushBatch`. A spend passes a
/// [`build_spend_tree`] instead; the pool rebuilds the rest of its image.
///
/// `digest` is the batch circuit's digest over the other 36 words; see
/// `domain::fiat_shamir`.
pub fn build_tu_batch_pub_inputs(
    start_index: u64,
    old_root: &Field,
    new_root: &Field,
    batch: &PaddedBatch,
    digest: U256,
) -> IMasp::TreeUpdateBatch {
    IMasp::TreeUpdateBatch {
        oldRoot: FixedBytes::<32>::from(*old_root),
        newRoot: FixedBytes::<32>::from(*new_root),
        startIndex: start_index,
        actualCount: batch.actual_count,
        cms: batch.cms,
        leafAsset: batch.leaf_asset,
        leafPublicIn: batch.leaf_public_in,
        isDeposit: batch.is_deposit,
        digest,
    }
}

/// Build a spend's `SpendTree`: the root its advance lands on, the position it
/// starts at, the ring slot of the root the transact proof names, and the batch
/// circuit's digest over the batch the spend implies.
pub fn build_spend_tree(
    start_index: u64,
    new_root: &Field,
    anchor_index: u8,
    digest: U256,
) -> IMasp::SpendTree {
    IMasp::SpendTree {
        newRoot: FixedBytes::<32>::from(*new_root),
        startIndex: start_index,
        anchorIndex: anchor_index,
        digest,
    }
}

pub fn build_one_aux(a: &OutputAuxDto) -> AppResult<IMasp::OutputAux> {
    Ok(IMasp::OutputAux {
        clueRx: parse_u256(&a.clue_r.x)?,
        clueRy: parse_u256(&a.clue_r.y)?,
        clueQx: parse_u256(&a.clue_q.x)?,
        clueQy: parse_u256(&a.clue_q.y)?,
        ephPubX: parse_u256(&a.eph_pub.x)?,
        ephPubY: parse_u256(&a.eph_pub.y)?,
        ciphertext: parse_hex_bytes(&a.ciphertext, "aux ciphertext")?,
    })
}

/// One aux payload per transact output leaf.
pub fn build_aux(
    aux: &[OutputAuxDto; TRANSACT_OUT],
) -> AppResult<[IMasp::OutputAux; TRANSACT_OUT]> {
    let built: Vec<IMasp::OutputAux> = aux.iter().map(build_one_aux).collect::<AppResult<_>>()?;
    built
        .try_into()
        .map_err(|_| AppError::Internal("aux arity".into()))
}

/// Map a wire deposit request into the on-chain struct. Used by the swap and
/// generic pipelines; the plain deposit path is wallet-driven and reaches the
/// relayer only through the flush flow.
pub fn build_deposit_request(d: &DepositRequestDto) -> AppResult<IMasp::DepositRequest> {
    Ok(IMasp::DepositRequest {
        chainId: U256::from(d.chain_id),
        publicAssetId: d.public_asset_id,
        publicIn: d.public_in,
        payer: parse_address(&d.payer)?,
        recipient: parse_address(&d.recipient)?,
        inner: parse_b32(&d.inner)?,
        feeAssetId: d.fee_asset_id,
        feeIn: d.fee_in,
        feeInner: parse_b32(&d.fee_inner)?,
    })
}
