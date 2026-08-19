//! Whether `flushBatch` would accept one pending deposit — decided locally,
//! before the prover runs.
//!
//! `flushBatch` is all-or-nothing, so one deposit the contract refuses costs
//! the whole batch a `tree_update_batch` Groth16 and lands nothing. The two
//! per-deposit guards in `_drainDeposit` are reproducible off-chain from the
//! escrow slot and the deposit's own fields, which is what this does.
//!
//! [`classify`] is pure on purpose: the decision table is the part worth
//! testing, and keeping the RPC read and the bookkeeping out of it means the
//! table can be tested without either.

use crate::domain::deposit_digest::{MAX_PUBLIC_IN, deposit_digest};
use crate::services::deposit_mempool::PendingDeposit;
use alloy::primitives::{Address, B256};

/// What pre-flight decided about one deposit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Include it in this batch.
    Flushable,
    /// Not the deposit's fault. Leave it out of this batch and look again
    /// next tick: nothing is charged and nothing is remembered.
    Skip(&'static str),
    /// It can never land, and the deposit's own fields prove it.
    Reject(&'static str),
    /// The replayed fields do not hash to the escrow slot. Damning only if
    /// [`deposit_digest`] is known to agree with this pool, which one deposit
    /// cannot establish — see `FlushPipeline::judge_mismatches`.
    DigestMismatch,
}

/// Judge `d` against `stored`, the digest the chain holds under its id.
pub fn classify(d: &PendingDeposit, stored: B256, masp: Address, chain_id: u64) -> Verdict {
    // `_drainDeposit` bounds this before narrowing to `uint48`, so an
    // oversized value reverts `PublicInTooLarge` however it is replayed.
    if d.public_in > MAX_PUBLIC_IN {
        return Verdict::Reject("public_in exceeds uint48");
    }
    // Zero is the contract's "no pending deposit" sentinel: canceled, or
    // flushed by someone else, with the indexer yet to write the row. Both
    // resolve on their own within a few blocks.
    if stored.is_zero() {
        return Verdict::Skip("escrow slot empty; already flushed or canceled");
    }
    if deposit_digest(masp, chain_id, d) != stored {
        return Verdict::DigestMismatch;
    }
    Verdict::Flushable
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    const MASP: Address = Address::new([0x11; 20]);
    const CHAIN: u64 = 31337;

    fn deposit() -> PendingDeposit {
        PendingDeposit {
            id: 1,
            cm: [0xab; 32],
            public_asset_id: 7,
            public_in: 1_000,
            fee_bps_at_submit: 25,
            payer: [0xcd; 20],
            submitted_at: 99,
            cv_dep: [U256::from(3), U256::from(4)],
            rcv: U256::from(5),
        }
    }

    /// The digest the chain would be holding for a well-formed deposit.
    fn escrowed(d: &PendingDeposit) -> B256 {
        deposit_digest(MASP, CHAIN, d)
    }

    #[test]
    fn a_deposit_matching_its_escrow_slot_is_flushable() {
        let d = deposit();
        assert_eq!(classify(&d, escrowed(&d), MASP, CHAIN), Verdict::Flushable);
    }

    #[test]
    fn an_empty_escrow_slot_is_skipped_not_held_against_the_deposit() {
        let d = deposit();
        assert!(matches!(
            classify(&d, B256::ZERO, MASP, CHAIN),
            Verdict::Skip(_)
        ));
    }

    #[test]
    fn a_replayed_field_that_does_not_hash_to_the_slot_is_a_mismatch() {
        let d = deposit();
        let stored = escrowed(&d);
        let mut stale = d.clone();
        // The fee changed after submit; the digest binds the submit-time one.
        stale.fee_bps_at_submit += 1;
        assert_eq!(
            classify(&stale, stored, MASP, CHAIN),
            Verdict::DigestMismatch
        );
    }

    /// The same deposit read against the wrong pool must not look flushable.
    #[test]
    fn a_slot_from_another_pool_is_a_mismatch() {
        let d = deposit();
        let stored = deposit_digest(Address::new([0x99; 20]), CHAIN, &d);
        assert_eq!(classify(&d, stored, MASP, CHAIN), Verdict::DigestMismatch);
    }

    /// Checked before the digest: an oversized `public_in` reverts on its own
    /// guard, and the contract would hash the narrowed value anyway.
    #[test]
    fn an_oversized_public_in_is_rejected_ahead_of_the_digest() {
        let mut d = deposit();
        d.public_in = MAX_PUBLIC_IN + 1;
        let stored = escrowed(&d);
        assert!(matches!(
            classify(&d, stored, MASP, CHAIN),
            Verdict::Reject(_)
        ));
    }

    #[test]
    fn the_largest_representable_public_in_is_still_flushable() {
        let mut d = deposit();
        d.public_in = MAX_PUBLIC_IN;
        assert_eq!(classify(&d, escrowed(&d), MASP, CHAIN), Verdict::Flushable);
    }
}
