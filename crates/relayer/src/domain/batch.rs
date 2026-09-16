//! The `tree_update_batch` circuit's leaf-indexed batch shape.
//!
//! Pure protocol constants and the padded leaf arrays every consumer builds from
//! them. No ABI and no I/O: `adapters::calldata` encodes the public arrays into
//! the on-chain struct, `domain::fiat_shamir` hashes them into the challenge, and
//! `services::witness` turns all of them into the prover's signals.

use crate::domain::deposit::EscrowLeaf;
use alloy::primitives::{FixedBytes, U256};

/// Maximum leaves per `tree_update_batch` proof, mirroring
/// `PubInputs.MAX_L_BATCH`. Counted in leaves rather than deposits: a deposit is
/// two leaves and a spend is `TRANSACT_OUT`.
pub const MAX_L_BATCH: usize = 8;

/// Leaves one deposit mints (mirrors `PubInputs.LEAVES_PER_DEPOSIT`): the
/// depositor's note, then the note paying whoever flushed it.
///
/// Widening `MAX_L_BATCH` requires a new trusted setup, since `COUNT_BITS`
/// pins it (3 at MAX_L 8), so the second leaf halves the deposits per batch
/// rather than adding slots.
pub const LEAVES_PER_DEPOSIT: usize = 2;

/// Deposits one `flushBatch` can carry.
pub const MAX_DEPOSITS_PER_BATCH: usize = MAX_L_BATCH / LEAVES_PER_DEPOSIT;

/// The batch circuit's leaf-indexed arrays, at full width.
///
/// One entry per leaf slot: the first `actual_count` are real and the rest are
/// zero padding that the circuit and the contract both enforce. Grouped into a
/// struct rather than six same-shaped arrays, since every consumer needs them
/// together and positional arguments of identical type transpose without a
/// compiler error. Holding `actual_count` here keeps the count from drifting from
/// the arrays it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddedBatch {
    pub cms: [FixedBytes<32>; MAX_L_BATCH],
    pub cv_deps: [[U256; 2]; MAX_L_BATCH],
    pub leaf_asset: [u64; MAX_L_BATCH],
    pub leaf_public_in: [u64; MAX_L_BATCH],
    /// `1` marks a deposit leaf, whose value commitment the circuit pins to
    /// its `(leaf_asset, leaf_public_in)`.
    pub is_deposit: [u8; MAX_L_BATCH],
    /// Each deposit leaf's `rcv_dep`, the blinder its value commitment is bound
    /// under. Private witness only: never in calldata or the challenge, and zero
    /// for spend leaves.
    pub rcv: [U256; MAX_L_BATCH],
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
            rcv: [U256::ZERO; MAX_L_BATCH],
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
    /// circuit checks against its value commitment and the blinder it is bound
    /// under.
    ///
    /// The caller supplies them flattened and in tree order — the depositor's
    /// note at `2i`, the relayer's fee note at `2i + 1` — which is the order
    /// `_drainDeposit` reads them back in. Both leaves of a deposit carry
    /// `is_deposit = 1` and share an asset.
    ///
    /// Unlike [`Self::from_spend`], a slice wider than the circuit would be
    /// truncated here while `actual_count` still counted every leaf, so the
    /// bound is asserted rather than discovered in the prover. Callers are
    /// clamped to `MAX_DEPOSITS_PER_BATCH` at boot.
    pub fn from_deposits(leaves: &[EscrowLeaf]) -> Self {
        debug_assert!(
            leaves.len() <= MAX_L_BATCH,
            "{} leaves exceeds the circuit's {MAX_L_BATCH} slots",
            leaves.len()
        );
        let mut batch = Self::zeroed();
        for (slot, d) in batch.slots_mut().zip(leaves) {
            *slot.cm = d.cm.into();
            *slot.cv_dep = d.cv_dep;
            *slot.leaf_asset = d.asset_id;
            *slot.leaf_public_in = d.public_in;
            *slot.is_deposit = 1;
            *slot.rcv = d.rcv;
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
            .zip(self.rcv.iter_mut())
            .map(
                |(((((cm, cv_dep), leaf_asset), leaf_public_in), is_deposit), rcv)| BatchSlot {
                    cm,
                    cv_dep,
                    leaf_asset,
                    leaf_public_in,
                    is_deposit,
                    rcv,
                },
            )
    }
}

/// One leaf slot borrowed across every array at once, so a write cannot land in
/// the wrong one.
struct BatchSlot<'a> {
    cm: &'a mut FixedBytes<32>,
    cv_dep: &'a mut [U256; 2],
    leaf_asset: &'a mut u64,
    leaf_public_in: &'a mut u64,
    is_deposit: &'a mut u8,
    rcv: &'a mut U256,
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
        let leaf = |cm: u8, public_in: u64| EscrowLeaf {
            cm: [cm; 32],
            cv_dep: [U256::from(1), U256::from(2)],
            asset_id: 7,
            public_in,
            rcv: U256::from(u64::from(cm)),
        };
        let batch = PaddedBatch::from_deposits(&[leaf(0xaa, 1_000), leaf(0xbb, 250)]);

        // Built at MAX_L_BATCH width rather than written out, so widening the
        // batch does not silently turn this into a length mismatch.
        let mut want_deposit = [0u8; MAX_L_BATCH];
        want_deposit[0] = 1;
        want_deposit[1] = 1;
        let mut want_public_in = [0u64; MAX_L_BATCH];
        want_public_in[0] = 1_000;
        want_public_in[1] = 250;

        assert_eq!(batch.actual_count, 2);
        assert_eq!(batch.is_deposit, want_deposit);
        // Padding stays zero; the contract and the circuit both enforce it.
        assert_eq!(batch.cms[2], FixedBytes::<32>::ZERO);
        assert_eq!(batch.leaf_public_in, want_public_in);
        assert_eq!(batch.rcv[1], U256::from(0xbbu64));
        assert_eq!(batch.rcv[2], U256::ZERO);
    }
}
