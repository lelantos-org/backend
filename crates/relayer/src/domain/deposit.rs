//! One pending escrowed deposit, as the flush path sees it.
//!
//! Pure: the row projection and the query that produces these live in
//! `repositories::deposit_escrowed_events`, while the digest derivation in
//! `domain::deposit_digest` and the decision table in
//! `services::pipeline::flush::preflight` read them.

use crate::domain::batch::LEAVES_PER_DEPOSIT;
use alloy::primitives::U256;
use serde_json::Value as JsonValue;

/// One escrowed deposit awaiting a flush.
///
/// A deposit occupies two leaves, the depositor's note and the note paying
/// whoever flushes it, so this carries a `cm`, `cv_dep` and `rcv` for each. The
/// fields are flat because they project the event row; [`PendingDeposit::leaves`]
/// pairs them in tree order, and the flush pipeline goes through it rather than
/// reading the fields directly.
#[derive(Debug, Clone)]
pub struct PendingDeposit {
    pub id: u64,
    pub cm: [u8; 32],
    pub public_asset_id: u64,
    pub public_in: u64,
    /// `feeBpsAtSubmit` from the `DepositEscrowed` event. Part of the on-chain
    /// digest preimage, which `flushBatch` re-derives from the `DepositMeta` the
    /// relayer replays; the contract stores only the digest.
    pub fee_bps_at_submit: u16,
    /// Digest preimage fields the contract does not keep. `submitted_at` is the
    /// `DepositEscrowed` block number, narrowed to the `uint32` the contract
    /// hashed.
    pub payer: [u8; 20],
    pub submitted_at: u32,
    pub cv_dep: [U256; 2],
    pub rcv: U256,
    /// The relayer's fee note: the second leaf the deposit mints, and what pays
    /// for the `flushBatch` gas.
    ///
    /// `fee_asset_id`, `fee_in`, `fee_cm` and `fee_cv_dep` are digest preimage and
    /// must read back exactly as escrowed. `fee_rcv` is not: it is the private
    /// blinder needed to build that leaf's batch witness. `fee_aux` is the
    /// encrypted payload the relayer trial-decrypts to learn what it is paid.
    ///
    /// `fee_asset_id` is chosen by the payer independently of `public_asset_id`,
    /// and is 0 exactly when `fee_in` is 0; read it through
    /// [`PendingDeposit::fee_asset`].
    pub fee_asset_id: u64,
    pub fee_in: u64,
    pub fee_cm: [u8; 32],
    pub fee_cv_dep: [U256; 2],
    pub fee_rcv: U256,
    pub fee_aux: JsonValue,
}

/// One of the two leaves a deposit mints.
///
/// The depositor's leaf is in the deposit's asset and the fee leaf in the fee
/// asset, which may differ, so `asset_id` is carried per leaf rather than looked
/// up by every consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscrowLeaf {
    pub cm: [u8; 32],
    pub cv_dep: [U256; 2],
    pub asset_id: u64,
    pub public_in: u64,
    /// The leaf's `rcv_dep`: private witness for the batch circuit's per-leaf
    /// deposit binding, never part of the escrow digest.
    pub rcv: U256,
}

impl PendingDeposit {
    /// The asset the fee note pays in, or `None` when it is worthless.
    ///
    /// `_validateDeposit` escrows `fee_asset_id == 0` exactly when `fee_in == 0`,
    /// so the two fields state one fact. Deciding it here keeps pricing, the
    /// fee-note check and the leaf witness on the same side of that rule.
    pub fn fee_asset(&self) -> Option<u64> {
        (self.fee_in != 0).then_some(self.fee_asset_id)
    }

    /// The asset this deposit's flush fee is priced in.
    ///
    /// The fee asset when the note carries value. A zero fee names no asset, and
    /// asset 0 cannot be priced, so it prices in the deposit's asset instead: on a
    /// charging relayer the deposit then defers as unpaid rather than being
    /// skipped as unpriceable, and re-judged, every tick.
    pub fn fee_pricing_asset(&self) -> u64 {
        self.fee_asset().unwrap_or(self.public_asset_id)
    }

    /// This deposit's leaves in the order `flushBatch` inserts them: the
    /// depositor's note, then the note paying whoever flushed it.
    ///
    /// Every leaf-indexed array the flush pipeline builds goes through here, so
    /// the order is decided once. `_drainDeposit` reads the pair back at `2i` and
    /// `2i + 1` and rejects the batch if either is not a deposit leaf, so a
    /// transposition costs the whole batch its proof.
    ///
    /// The fee leaf names the fee asset, which the escrow digest binds: any other
    /// `leafAsset` reverts `DigestMismatch`. A worthless fee note (e.g. a swap's
    /// output escrow) names asset 0, as the batch circuit's `leaf_asset == 0 iff
    /// leaf_public_in == 0` requires.
    pub fn leaves(&self) -> [EscrowLeaf; LEAVES_PER_DEPOSIT] {
        [
            EscrowLeaf {
                cm: self.cm,
                cv_dep: self.cv_dep,
                asset_id: self.public_asset_id,
                public_in: self.public_in,
                rcv: self.rcv,
            },
            EscrowLeaf {
                cm: self.fee_cm,
                cv_dep: self.fee_cv_dep,
                asset_id: self.fee_asset().unwrap_or(0),
                public_in: self.fee_in,
                rcv: self.fee_rcv,
            },
        ]
    }

    /// A well-formed deposit whose fee note is paid in another asset (9) than the
    /// deposit's (7). Shared by the flush path's unit tests, which vary it one
    /// field at a time.
    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        PendingDeposit {
            id: 1,
            cm: [0xaa; 32],
            public_asset_id: 7,
            public_in: 1_000,
            fee_bps_at_submit: 25,
            payer: [0xcd; 20],
            submitted_at: 99,
            cv_dep: [U256::from(1), U256::from(2)],
            rcv: U256::from(3),
            fee_asset_id: 9,
            fee_in: 250,
            fee_cm: [0xbb; 32],
            fee_cv_dep: [U256::from(4), U256::from(5)],
            fee_rcv: U256::from(6),
            fee_aux: JsonValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is what `_drainDeposit` reads back at `2i` and `2i + 1`. Swapping
    /// the pair builds a batch that proves and then reverts, so it is pinned here
    /// rather than at each call site.
    #[test]
    fn test_leaves_puts_the_depositors_note_before_the_fee_note() {
        let d = PendingDeposit::fixture();
        let [principal, fee] = d.leaves();

        assert_eq!(principal.cm, d.cm);
        assert_eq!(principal.public_in, d.public_in);
        assert_eq!(principal.rcv, d.rcv);

        assert_eq!(fee.cm, d.fee_cm);
        assert_eq!(fee.public_in, d.fee_in);
        assert_eq!(fee.rcv, d.fee_rcv);
    }

    /// A valued fee note is in its own escrowed asset, which the digest binds and
    /// which need not be the deposit's.
    #[test]
    fn test_leaves_a_cross_asset_fee_note_names_the_fee_asset() {
        let d = PendingDeposit::fixture();
        let [principal, fee] = d.leaves();
        assert_eq!(principal.asset_id, 7);
        assert_eq!(fee.asset_id, 9);
    }

    /// A zero-value fee note must name asset 0, or the batch circuit's
    /// `asset == 0 iff value == 0` constraint rejects the witness.
    #[test]
    fn test_leaves_a_worthless_fee_note_names_asset_zero() {
        let d = zero_fee();
        let [principal, fee] = d.leaves();

        assert_eq!(principal.asset_id, d.public_asset_id);
        assert_eq!(fee.asset_id, 0);
    }

    #[test]
    fn test_fee_pricing_asset_a_valued_fee_is_priced_in_the_fee_asset() {
        let d = PendingDeposit::fixture();
        assert_eq!(d.fee_asset(), Some(9));
        assert_eq!(d.fee_pricing_asset(), 9);
    }

    /// Asset 0 cannot be priced, so a charging relayer would skip the deposit as
    /// unpriceable every tick instead of deferring it as unpaid.
    #[test]
    fn test_fee_pricing_asset_a_zero_fee_is_priced_in_the_deposits_asset() {
        let d = zero_fee();
        assert_eq!(d.fee_asset(), None);
        assert_eq!(d.fee_pricing_asset(), d.public_asset_id);
    }

    /// The shape `_validateDeposit` escrows for an unpaid deposit.
    fn zero_fee() -> PendingDeposit {
        PendingDeposit {
            fee_asset_id: 0,
            fee_in: 0,
            ..PendingDeposit::fixture()
        }
    }
}
