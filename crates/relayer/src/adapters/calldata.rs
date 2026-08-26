//! Builders mapping relayer DTOs and tree state into the on-chain `IMasp`
//! argument structs. Pure conversions, no I/O.

use crate::adapters::abi::IMasp;
use crate::adapters::parse::{parse_address, parse_b32, parse_u256};
use crate::domain::dto::{
    DepositRequestDto, OutputAuxDto, PointDto, ProofDto, PubInputsDto, TRANSACT_IN, TRANSACT_OUT,
};
use crate::domain::error::AppResult;
use crate::services::prover::TreeUpdateBatchProof;
use alloy::primitives::{FixedBytes, U256};
use fmd_crypto::tree::Field;

/// Maximum leaves per `tree_update_batch` proof, mirroring
/// `PubInputs.MAX_L_BATCH`. Counted in leaves rather than deposits: a deposit is
/// two leaves and a spend is `TRANSACT_OUT`.
pub const MAX_L_BATCH: usize = 4;

/// Leaves one deposit mints (mirrors `PubInputs.LEAVES_PER_DEPOSIT`): the
/// depositor's note, then the note paying whoever flushed it.
///
/// Widening `MAX_L_BATCH` requires a new trusted setup, since `COUNT_BITS = 2`
/// pins it, so the second leaf halves the deposits per batch rather than adding
/// slots.
pub const LEAVES_PER_DEPOSIT: usize = 2;

/// Deposits one `flushBatch` can carry.
pub const MAX_DEPOSITS_PER_BATCH: usize = MAX_L_BATCH / LEAVES_PER_DEPOSIT;

/// The batch circuit's leaf-indexed arrays, at full width.
///
/// One entry per leaf slot: the first `actual_count` are real and the rest are
/// zero padding that the circuit and the contract both enforce. Grouped into a
/// struct rather than five same-shaped arrays, since every consumer needs all of
/// them and positional arguments of identical type transpose without a compiler
/// error. Holding `actual_count` here keeps the count from drifting from the
/// arrays it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddedBatch {
    pub cms: [FixedBytes<32>; MAX_L_BATCH],
    pub cv_deps: [[U256; 2]; MAX_L_BATCH],
    pub leaf_asset: [u64; MAX_L_BATCH],
    pub leaf_public_in: [u64; MAX_L_BATCH],
    /// `1` marks a deposit leaf, whose value commitment the circuit pins to
    /// its `(leaf_asset, leaf_public_in)`.
    pub is_deposit: [u8; MAX_L_BATCH],
    /// How many leading slots are real. A leaf count rather than a pair count, so
    /// an odd value is valid.
    pub actual_count: u64,
}

impl PaddedBatch {
    fn zeroed() -> Self {
        Self {
            cms: [FixedBytes::<32>::ZERO; MAX_L_BATCH],
            cv_deps: [[U256::ZERO; 2]; MAX_L_BATCH],
            leaf_asset: [0; MAX_L_BATCH],
            leaf_public_in: [0; MAX_L_BATCH],
            is_deposit: [0; MAX_L_BATCH],
            actual_count: 0,
        }
    }

    /// Spend leaves. Every deposit-only field stays zero: the transact SNARK
    /// already proves conservation, so the per-leaf deposit binding is skipped.
    ///
    /// # Panics
    /// If more leaves are supplied than the circuit has slots. Callers are
    /// fixed-arity (`TRANSACT_OUT`) or clamped to `MAX_L_BATCH` at boot.
    pub fn from_spend(cms: &[FixedBytes<32>], cv_deps: &[[U256; 2]]) -> Self {
        assert_eq!(cms.len(), cv_deps.len(), "one cv_dep per commitment");
        let mut batch = Self::zeroed();
        batch.cms[..cms.len()].copy_from_slice(cms);
        batch.cv_deps[..cv_deps.len()].copy_from_slice(cv_deps);
        batch.actual_count = cms.len() as u64;
        batch
    }

    /// Deposit leaves, two per escrowed deposit, each carrying the binding the
    /// circuit checks against its value commitment.
    ///
    /// The caller supplies them flattened and in tree order — the depositor's
    /// note at `2i`, the relayer's fee note at `2i + 1` — which is the order
    /// `_drainDeposit` reads them back in. Both leaves of a deposit carry
    /// `is_deposit = 1` and share an asset.
    ///
    /// # Panics
    /// As [`Self::from_spend`].
    pub fn from_deposits(leaves: &[DepositLeaf]) -> Self {
        let mut batch = Self::zeroed();
        for (slot, d) in batch.slots_mut().zip(leaves) {
            *slot.cm = d.cm;
            *slot.cv_dep = d.cv_dep;
            *slot.leaf_asset = d.leaf_asset;
            *slot.leaf_public_in = d.leaf_public_in;
            *slot.is_deposit = 1;
        }
        batch.actual_count = leaves.len() as u64;
        batch
    }

    fn slots_mut(&mut self) -> impl Iterator<Item = BatchSlot<'_>> {
        self.cms
            .iter_mut()
            .zip(self.cv_deps.iter_mut())
            .zip(self.leaf_asset.iter_mut())
            .zip(self.leaf_public_in.iter_mut())
            .zip(self.is_deposit.iter_mut())
            .map(
                |((((cm, cv_dep), leaf_asset), leaf_public_in), is_deposit)| BatchSlot {
                    cm,
                    cv_dep,
                    leaf_asset,
                    leaf_public_in,
                    is_deposit,
                },
            )
    }
}

/// One leaf slot borrowed across all five arrays at once, so a write cannot land
/// in the wrong one.
struct BatchSlot<'a> {
    cm: &'a mut FixedBytes<32>,
    cv_dep: &'a mut [U256; 2],
    leaf_asset: &'a mut u64,
    leaf_public_in: &'a mut u64,
    is_deposit: &'a mut u8,
}

/// One escrowed deposit's contribution to a batch.
#[derive(Debug, Clone, Copy)]
pub struct DepositLeaf {
    pub cm: FixedBytes<32>,
    pub cv_dep: [U256; 2],
    pub leaf_asset: u64,
    pub leaf_public_in: u64,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the constructor guard in `MASP.sol`. These three constants describe
    /// one circuit shape, and `MAX_L_BATCH` cannot move without a new trusted
    /// setup, so a batch sized against a stale pair would be built and proven
    /// before being rejected on chain.
    #[test]
    fn test_batch_constants_describe_one_circuit_shape() {
        assert_eq!(MAX_DEPOSITS_PER_BATCH * LEAVES_PER_DEPOSIT, MAX_L_BATCH);
    }

    /// Both leaves of a deposit must be marked as deposit leaves: the circuit
    /// gates its per-leaf value binding on `is_deposit`, so a fee leaf left at
    /// zero would leave its `cv_dep` unconstrained.
    #[test]
    fn test_from_deposits_marks_every_supplied_leaf_as_a_deposit() {
        let leaf = |cm: u8, public_in: u64| DepositLeaf {
            cm: FixedBytes::<32>::repeat_byte(cm),
            cv_dep: [U256::from(1), U256::from(2)],
            leaf_asset: 7,
            leaf_public_in: public_in,
        };
        let batch = PaddedBatch::from_deposits(&[leaf(0xaa, 1_000), leaf(0xbb, 250)]);

        assert_eq!(batch.actual_count, 2);
        assert_eq!(batch.is_deposit, [1, 1, 0, 0]);
        // Padding stays zero; the contract and the circuit both enforce it.
        assert_eq!(batch.cms[2], FixedBytes::<32>::ZERO);
        assert_eq!(batch.leaf_public_in, [1_000, 250, 0, 0]);
    }
}
