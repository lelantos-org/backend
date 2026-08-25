//! Whether `flushBatch` would accept one pending deposit — decided locally,
//! before the prover runs.
//!
//! `flushBatch` is all-or-nothing, so one deposit the contract refuses costs
//! the whole batch a `tree_update_batch` Groth16 and lands nothing. The two
//! per-deposit guards in `_drainDeposit` are reproducible off-chain from the
//! escrow slot and the deposit's own fields, which is what this does.
//!
//! One guard here is the circuit's rather than the contract's: `rcv` never
//! enters the escrow digest, so `flushBatch` would accept a blinder the batch
//! circuit cannot witness. That failure lands even earlier — in witness
//! generation — and costs the same whole batch, so it belongs in the same
//! table.
//!
//! [`classify`] is pure on purpose: the decision table is the part worth
//! testing, and keeping the RPC read and the bookkeeping out of it means the
//! table can be tested without either.

use crate::domain::deposit_digest::{MAX_PUBLIC_IN, deposit_digest};
use crate::services::deposit_fee::FeeNote;
use crate::services::deposit_mempool::PendingDeposit;
use alloy::primitives::{Address, B256};

/// `rcv` is decomposed by `Num2Bits(RCV_BITS)` inside `MulH`, with
/// `RCV_BITS = 252` (`circuits/src/lib/value_commit.circom`). A wider blinder
/// has no witness, so the deposit can never be proven — and the contract
/// cannot screen for it, since `rcv` is not part of the escrow digest.
///
/// The bound narrowed from 253 bits when the batch circuit moved to
/// `FixedBaseMul`; a deposit escrowed under the old bound with `rcv` in
/// `[2^252, 2^253)` is stranded and must be reclaimed with `cancelDeposit`.
const RCV_BITS: usize = 252;

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

/// Whether this relayer charges for the flush, and what it found in the
/// deposit's fee leaf.
///
/// Arrives already computed so [`classify`] stays pure — the trial decryption
/// and the gas quote are the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeeGate {
    /// This relayer takes a fee: the deposit's fee leaf must be a note it owns
    /// and worth at least `required` circuit units of the deposit's asset.
    Charged { note: FeeNote, required: u64 },
    /// No shielded fee is configured for this chain, so flushes are subsidised
    /// and the fee leaf is not inspected at all. It is still minted and still
    /// spendable — by whoever the payer addressed it to.
    ///
    /// Without this a subsidised chain would stall completely: with no viewing
    /// key there is nothing to decrypt with, every deposit would read as
    /// `NotOurs`, and nothing would ever flush.
    Subsidised,
    /// The fee could not be priced at all — an asset this relayer will not
    /// take, or an oracle that is down. Distinct from an unpaid one: the
    /// deposit may be perfectly funded and the fault entirely this relayer's,
    /// so it must not be recorded as anything the deposit did wrong.
    Unpriceable,
}

/// Judge `d` against `stored`, the digest the chain holds under its id.
pub fn classify(
    d: &PendingDeposit,
    stored: B256,
    masp: Address,
    chain_id: u64,
    fee: &FeeGate,
) -> Verdict {
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
    // Checked after the digest so a mismatched replay is still reported as
    // one: an `rcv` that did not come from the escrowed deposit says nothing
    // about the deposit itself.
    if d.rcv.bit_len() > RCV_BITS {
        return Verdict::Reject("rcv exceeds the circuit's 252-bit blinder");
    }
    // The same bound applies to the fee leaf's blinder: it is witnessed by the
    // same `MulH`, and the contract cannot screen for it either.
    if d.fee_rcv.bit_len() > RCV_BITS {
        return Verdict::Reject("fee_rcv exceeds the circuit's 252-bit blinder");
    }

    // Every fee outcome is a `Skip`, never a `Reject`. A deposit this relayer
    // will not flush is still perfectly flushable by the relayer it actually
    // pays, and by the payer themselves — `flushBatch` is permissionless.
    // Rejecting would be this relayer asserting something about a deposit that
    // is none of its business, and `Reject` is remembered where `Skip` is not.
    match fee {
        FeeGate::Subsidised => Verdict::Flushable,
        FeeGate::Unpriceable => Verdict::Skip("cannot price the flush for this asset"),
        FeeGate::Charged { note, required } => match note {
            FeeNote::NotOurs => Verdict::Skip("fee note is not addressed to this relayer"),
            FeeNote::Malformed(why) => Verdict::Skip(why),
            FeeNote::Paid { paid } if paid < required => {
                Verdict::Skip("fee note does not cover the flush")
            }
            FeeNote::Paid { .. } => Verdict::Flushable,
        },
    }
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
            fee_in: 250,
            fee_cm: [0xef; 32],
            fee_cv_dep: [U256::from(6), U256::from(7)],
            fee_rcv: U256::from(8),
            fee_aux: serde_json::Value::Null,
        }
    }

    /// A fee leaf that is ours and covers the flush — the ordinary case, so
    /// the tests below vary one thing at a time against it.
    fn paid() -> FeeGate {
        FeeGate::Charged {
            note: FeeNote::Paid { paid: 250 },
            required: 250,
        }
    }

    /// The digest the chain would be holding for a well-formed deposit.
    fn escrowed(d: &PendingDeposit) -> B256 {
        deposit_digest(MASP, CHAIN, d)
    }

    #[test]
    fn a_deposit_matching_its_escrow_slot_is_flushable() {
        let d = deposit();
        assert_eq!(
            classify(&d, escrowed(&d), MASP, CHAIN, &paid()),
            Verdict::Flushable
        );
    }

    #[test]
    fn an_empty_escrow_slot_is_skipped_not_held_against_the_deposit() {
        let d = deposit();
        assert!(matches!(
            classify(&d, B256::ZERO, MASP, CHAIN, &paid()),
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
            classify(&stale, stored, MASP, CHAIN, &paid()),
            Verdict::DigestMismatch
        );
    }

    /// The same deposit read against the wrong pool must not look flushable.
    #[test]
    fn a_slot_from_another_pool_is_a_mismatch() {
        let d = deposit();
        let stored = deposit_digest(Address::new([0x99; 20]), CHAIN, &d);
        assert_eq!(
            classify(&d, stored, MASP, CHAIN, &paid()),
            Verdict::DigestMismatch
        );
    }

    /// Checked before the digest: an oversized `public_in` reverts on its own
    /// guard, and the contract would hash the narrowed value anyway.
    #[test]
    fn an_oversized_public_in_is_rejected_ahead_of_the_digest() {
        let mut d = deposit();
        d.public_in = MAX_PUBLIC_IN + 1;
        let stored = escrowed(&d);
        assert!(matches!(
            classify(&d, stored, MASP, CHAIN, &paid()),
            Verdict::Reject(_)
        ));
    }

    /// A blinder the batch circuit cannot decompose strands the deposit: the
    /// contract would accept it, so nothing on-chain screens it out and every
    /// flush tick that includes it fails in witness generation.
    #[test]
    fn an_rcv_wider_than_the_circuit_blinder_is_rejected() {
        let mut d = deposit();
        d.rcv = U256::from(1u8) << RCV_BITS;
        let stored = escrowed(&d);
        assert!(matches!(
            classify(&d, stored, MASP, CHAIN, &paid()),
            Verdict::Reject(_)
        ));
    }

    /// The widest blinder `Num2Bits(252)` still witnesses.
    #[test]
    fn the_widest_representable_rcv_is_still_flushable() {
        let mut d = deposit();
        d.rcv = (U256::from(1u8) << RCV_BITS) - U256::from(1u8);
        let stored = escrowed(&d);
        assert_eq!(
            classify(&d, stored, MASP, CHAIN, &paid()),
            Verdict::Flushable
        );
    }

    /// `rcv` is not in the escrow preimage, so a replay that disagrees with
    /// the chain must still be reported as a mismatch rather than blamed on
    /// the blinder.
    #[test]
    fn an_oversized_rcv_does_not_mask_a_digest_mismatch() {
        let mut d = deposit();
        let stored = escrowed(&d);
        d.rcv = U256::from(1u8) << RCV_BITS;
        d.public_asset_id += 1;
        assert_eq!(
            classify(&d, stored, MASP, CHAIN, &paid()),
            Verdict::DigestMismatch
        );
    }

    #[test]
    fn the_largest_representable_public_in_is_still_flushable() {
        let mut d = deposit();
        d.public_in = MAX_PUBLIC_IN;
        assert_eq!(
            classify(&d, escrowed(&d), MASP, CHAIN, &paid()),
            Verdict::Flushable
        );
    }

    /// The fee leaf's blinder is witnessed by the same `MulH` as the
    /// depositor's, and the contract screens neither.
    #[test]
    fn a_fee_rcv_wider_than_the_circuit_blinder_is_rejected() {
        let mut d = deposit();
        d.fee_rcv = U256::from(1u8) << RCV_BITS;
        let stored = escrowed(&d);
        assert!(matches!(
            classify(&d, stored, MASP, CHAIN, &paid()),
            Verdict::Reject(_)
        ));
    }

    /// Not this relayer's note. Someone else can flush it, and the payer can
    /// cancel it, so this must never be quarantined.
    #[test]
    fn a_fee_note_addressed_elsewhere_is_skipped_not_rejected() {
        let d = deposit();
        let gate = FeeGate::Charged {
            note: FeeNote::NotOurs,
            required: 250,
        };
        assert!(matches!(
            classify(&d, escrowed(&d), MASP, CHAIN, &gate),
            Verdict::Skip(_)
        ));
    }

    /// The approved behaviour for an underpaid deposit: decline to flush and
    /// leave it, rather than flush at a loss or quarantine it.
    #[test]
    fn a_fee_note_that_does_not_cover_the_flush_is_skipped() {
        let d = deposit();
        let gate = FeeGate::Charged {
            note: FeeNote::Paid { paid: 249 },
            required: 250,
        };
        assert!(matches!(
            classify(&d, escrowed(&d), MASP, CHAIN, &gate),
            Verdict::Skip(_)
        ));
    }

    /// Exactly the required amount is enough — the grace is already applied to
    /// `required`, so this bound must not be strict on the other side.
    #[test]
    fn a_fee_note_worth_exactly_the_required_amount_is_flushable() {
        let d = deposit();
        let gate = FeeGate::Charged {
            note: FeeNote::Paid { paid: 250 },
            required: 250,
        };
        assert_eq!(
            classify(&d, escrowed(&d), MASP, CHAIN, &gate),
            Verdict::Flushable
        );
    }

    /// A payload that decrypts for us but describes a different leaf. Skipped
    /// rather than rejected for the same reason as `NotOurs`: the payer can
    /// still cancel, and nothing about it stops another relayer.
    #[test]
    fn a_malformed_fee_note_is_skipped() {
        let d = deposit();
        let gate = FeeGate::Charged {
            note: FeeNote::Malformed("fee note rcv_dep is not the escrowed feeRcv"),
            required: 250,
        };
        assert!(matches!(
            classify(&d, escrowed(&d), MASP, CHAIN, &gate),
            Verdict::Skip(_)
        ));
    }

    /// A pricing failure is the relayer's problem, not the deposit's, so it
    /// must never be quarantined over one.
    #[test]
    fn a_deposit_whose_flush_cannot_be_priced_is_skipped() {
        let d = deposit();
        assert!(matches!(
            classify(&d, escrowed(&d), MASP, CHAIN, &FeeGate::Unpriceable),
            Verdict::Skip(_)
        ));
    }

    /// A subsidised chain has no viewing key, so every deposit would read as
    /// `NotOurs` and nothing would ever flush. The gate is skipped entirely.
    #[test]
    fn a_subsidised_chain_flushes_without_inspecting_the_fee_leaf() {
        let d = deposit();
        assert_eq!(
            classify(&d, escrowed(&d), MASP, CHAIN, &FeeGate::Subsidised),
            Verdict::Flushable
        );
    }

    /// Fee outcomes are judged only after the digest, so an unpayable fee
    /// never masks a replay that did not come from this deposit at all.
    #[test]
    fn a_digest_mismatch_outranks_an_unpaid_fee() {
        let d = deposit();
        let stored = escrowed(&d);
        let mut stale = d.clone();
        stale.fee_bps_at_submit += 1;
        let gate = FeeGate::Charged {
            note: FeeNote::NotOurs,
            required: 250,
        };
        assert_eq!(
            classify(&stale, stored, MASP, CHAIN, &gate),
            Verdict::DigestMismatch
        );
    }
}
