//! One pending escrowed deposit, as the flush path sees it.
//!
//! Pure: the row projection and the query that produces these live in
//! `services::deposit_mempool`, while the digest derivation in
//! `domain::deposit_digest` and the decision table in
//! `services::pipeline::deposit_preflight` read them.

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
    /// `fee_in`, `fee_cm` and `fee_cv_dep` are digest preimage and must read back
    /// exactly as escrowed. `fee_rcv` is not: it is the private blinder needed to
    /// build that leaf's batch witness. `fee_aux` is the encrypted payload the
    /// relayer trial-decrypts to learn what it is paid.
    pub fee_in: u64,
    pub fee_cm: [u8; 32],
    pub fee_cv_dep: [U256; 2],
    pub fee_rcv: U256,
    pub fee_aux: JsonValue,
}

/// One of the two leaves a deposit mints.
///
/// Both are denominated in the deposit's own asset, which `_drainDeposit`
/// requires, so `asset_id` is carried per leaf rather than looked up by every
/// consumer.
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
    /// This deposit's leaves in the order `flushBatch` inserts them: the
    /// depositor's note, then the note paying whoever flushed it.
    ///
    /// Every leaf-indexed array the flush pipeline builds goes through here, so
    /// the order is decided once. `_drainDeposit` reads the pair back at `2i` and
    /// `2i + 1` and rejects the batch if either is not a deposit leaf, so a
    /// transposition costs the whole batch its proof.
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
                asset_id: self.public_asset_id,
                public_in: self.fee_in,
                rcv: self.fee_rcv,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deposit() -> PendingDeposit {
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
            fee_in: 250,
            fee_cm: [0xbb; 32],
            fee_cv_dep: [U256::from(4), U256::from(5)],
            fee_rcv: U256::from(6),
            fee_aux: JsonValue::Null,
        }
    }

    /// The order is what `_drainDeposit` reads back at `2i` and `2i + 1`. Swapping
    /// the pair builds a batch that proves and then reverts, so it is pinned here
    /// rather than at each call site.
    #[test]
    fn test_leaves_puts_the_depositors_note_before_the_fee_note() {
        let d = deposit();
        let [principal, fee] = d.leaves();

        assert_eq!(principal.cm, d.cm);
        assert_eq!(principal.public_in, d.public_in);
        assert_eq!(principal.rcv, d.rcv);

        assert_eq!(fee.cm, d.fee_cm);
        assert_eq!(fee.public_in, d.fee_in);
        assert_eq!(fee.rcv, d.fee_rcv);
    }

    /// `_drainDeposit` requires both leaves to name the deposit's asset, and the
    /// fee note has no asset field of its own.
    #[test]
    fn test_both_leaves_carry_the_deposits_asset() {
        let d = deposit();
        assert!(d.leaves().iter().all(|l| l.asset_id == d.public_asset_id));
    }
}
