//! Verifying that a pending deposit pays this relayer for flushing it.
//!
//! Every deposit mints two leaves: the depositor's note and a note addressed
//! to a shielded address the payer chose. If that address is this relayer's,
//! the second leaf is what pays for `flushBatch` gas.
//!
//! # Why `feeIn` alone is not enough
//!
//! `feeIn` is public, so the amount needs no decryption. What must be
//! established is that the note is ours and spendable, neither of which is
//! visible on chain: a payer can escrow `feeIn = 10_000` against a note
//! addressed to themselves, producing a deposit that looks funded and pays
//! nobody. Two checks make the leaf actionable:
//!
//! 1. The owner half is rebuilt against this relayer's own `pk`. `feeInner` is
//!    escrow digest preimage, so it is the value the payer signed a Permit2
//!    witness over and neither a relayer nor a flusher can vary it. Rebuilding
//!    `Poseidon(TAG_INNER, pk, rho, rcm)` from the decrypted plaintext fails
//!    for a note owned by someone else. The note must also be sealed as its
//!    seed dictates, or this relayer's wallet drops it; see
//!    [`FeeRecipient::open`].
//! 2. The plaintext must agree with the escrow: `value` with `feeIn`, and a
//!    valued note's `asset_id` with the escrowed `feeAssetId`, which may differ
//!    from the deposit's `publicAssetId`. The batch circuit builds the leaf as
//!    `Poseidon(TAG_CM, feeAssetId·2^64 + feeIn, feeInner)`, so the note is
//!    worth the escrowed amount whatever the plaintext says; but a wallet
//!    recognises a note by rebuilding its leaf from the plaintext, so one that
//!    states another amount is a note this relayer's wallet never finds. A
//!    worthless note is exempt from the asset check: it pays nothing in any
//!    asset.
//!
//! Only `ivk` is required, so the spending key that could move collected fees
//! never exists on this host, as on the spend path.

use crate::domain::deposit::PendingDeposit;
use crate::domain::error::{AppError, AppResult};
use crate::services::fees::shielded::FeeRecipient;
use crate::services::fees::shielded::recipient::{OpenedNote, point_of};
use crypto::note;
use serde::Deserialize;

/// What the fee leaf of one deposit turned out to be.
///
/// `NotOurs` and `Malformed` are distinct even though both lead to the same
/// verdict: `NotOurs` is the ordinary case of a deposit addressed to a different
/// relayer, while `Malformed` means the payload and the escrow disagree, which is
/// worth logging because a wallet producing them strands its users' funds until
/// they cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeeNote {
    /// Ours, spendable, and worth `paid` circuit units of the fee asset.
    Paid { paid: u64 },
    /// Did not decrypt to a note this relayer owns. A foreign note, a pad and a
    /// corrupt ciphertext are indistinguishable here by design.
    NotOurs,
    /// Decrypted for us, but does not describe the leaf that was escrowed.
    Malformed(&'static str),
}

/// The `fee_aux` JSON the indexer wrote from the `DepositEscrowed` log.
///
/// Field names match `explorer-indexer`'s `encode_aux`; the values are decimal
/// strings and a `0x` ciphertext, exactly as they appear in the event.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeeAux {
    clue_rx: String,
    clue_ry: String,
    eph_pub_x: String,
    eph_pub_y: String,
    ciphertext: String,
}

/// Decide what `d`'s fee leaf pays this relayer.
///
/// Pure: no network, database or clock. `Err` is reserved for a `fee_aux` column
/// that could not be parsed at all, which is a pipeline problem rather than a fee
/// one.
pub fn assess(recipient: &FeeRecipient, d: &PendingDeposit) -> AppResult<FeeNote> {
    let aux: FeeAux = serde_json::from_value(d.fee_aux.clone())
        .map_err(|e| AppError::Internal(format!("deposit {}: fee_aux is unreadable: {e}", d.id)))?;

    let Some(plain) = open(recipient, &aux)? else {
        return Ok(FeeNote::NotOurs);
    };

    // Rebuilt against this relayer's own `pk`, so a note merely encrypted to us
    // does not pass.
    let inner = note::inner(recipient.pk(), &plain.rho, &plain.rcm)
        .map_err(|e| AppError::Internal(format!("deposit {}: note inner: {e}", d.id)))?;
    if inner != d.fee_inner {
        return Ok(FeeNote::NotOurs);
    }

    // Past this point the note is provably ours, so a disagreement with the escrow
    // is the payer's fault rather than another party's note.
    // A worthless note has no fee asset to disagree with; see the module docs.
    if d.fee_asset().is_some_and(|asset| asset != plain.asset_id) {
        return Ok(FeeNote::Malformed("fee note names a different asset"));
    }
    if plain.value != d.fee_in {
        return Ok(FeeNote::Malformed("fee note value disagrees with feeIn"));
    }

    // The leaf's value is the escrowed one: the batch circuit builds it from
    // `(feeAssetId, feeIn, feeInner)`.
    Ok(FeeNote::Paid { paid: d.fee_in })
}

fn open(recipient: &FeeRecipient, aux: &FeeAux) -> AppResult<Option<OpenedNote>> {
    let wire = hex_bytes(&aux.ciphertext)?;
    let epk = point_of(&aux.eph_pub_x, &aux.eph_pub_y, "fee_aux.ephPub", 0)?;
    let clue_r = point_of(&aux.clue_rx, &aux.clue_ry, "fee_aux.clueR", 0)?;
    Ok(recipient.open(epk, clue_r, &wire))
}

fn hex_bytes(s: &str) -> AppResult<Vec<u8>> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|e| AppError::Internal(format!("fee_aux ciphertext is not hex: {e}")))
}

#[cfg(test)]
mod tests {
    //! The same vectors the spend path uses. The deposit path derives `rho`
    //! freely rather than from a nullifier, so of a spend slot only the fields
    //! unaffected by that difference are used: the plaintext, its `inner` and
    //! the ephemeral key.

    use super::*;
    use crate::adapters::parse::{FieldRef, parse_field};
    use crate::domain::dto::OutputAuxDto;
    use crate::services::fees::shielded::fixture::{Fixture, fixture, recipient};
    use crypto::tree::Field;
    use serde_json::json;

    fn field(s: &str) -> Field {
        parse_field(s, FieldRef::Named("fixture field"))
            .expect("fixture field parses")
            .0
    }

    fn aux_json(a: &OutputAuxDto) -> serde_json::Value {
        json!({
            "clueRx": a.clue_r.x,
            "clueRy": a.clue_r.y,
            "ephPubX": a.eph_pub.x,
            "ephPubY": a.eph_pub.y,
            "ciphertext": a.ciphertext,
        })
    }

    /// A deposit whose fee leaf is the fixture's deposit note, escrowed correctly.
    fn deposit_paying(f: &Fixture) -> PendingDeposit {
        let plain = f.plaintext_of(&f.deposit_fee.aux).expect("decrypts");
        PendingDeposit {
            public_asset_id: f.asset_id,
            fee_asset_id: f.asset_id,
            fee_in: plain.value,
            fee_inner: field(&f.deposit_fee.inner),
            fee_aux: aux_json(&f.deposit_fee.aux),
            ..PendingDeposit::fixture()
        }
    }

    /// The leaf a flush mints for the fixture's fee note is the commitment the
    /// relayer's wallet rebuilds from the plaintext, so the note is spendable as
    /// the escrowed amount. Read independently of `assess`.
    #[test]
    fn test_the_fixture_deposit_note_opens_the_leaf_the_batch_circuit_builds() {
        let f = fixture();
        let r = recipient(&f);
        let plain = f.plaintext_of(&f.deposit_fee.aux).expect("decrypts");
        let d = deposit_paying(&f);
        let leaf = note::commitment_from_inner(d.fee_asset_id, d.fee_in, &d.fee_inner)
            .expect("canonical inner");
        assert_eq!(leaf, field(&f.deposit_fee.cm));
        let rcm = note::expand_seed(&plain.rseed, &plain.rho).rcm;
        assert_eq!(
            note::commitment(plain.asset_id, plain.value, r.pk(), &plain.rho, &rcm)
                .expect("commitment"),
            leaf
        );
    }

    #[test]
    fn test_assess_a_correctly_escrowed_note_returns_paid() {
        let f = fixture();
        let r = recipient(&f);
        let d = deposit_paying(&f);
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Paid { paid: 250 }
        );
    }

    /// This note decrypts for us but its `inner` was built against another
    /// party's `pk`, so it is not ours to spend.
    #[test]
    fn test_assess_a_note_owned_by_another_key_is_not_ours() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        d.fee_inner = field(&f.foreign_owner.inner);
        d.fee_aux = aux_json(&f.foreign_owner.aux);
        assert_eq!(assess(&r, &d).expect("readable"), FeeNote::NotOurs);
    }

    /// Encrypted to a different recipient: it does not decrypt, which is
    /// indistinguishable from a pad by design.
    #[test]
    fn test_assess_a_note_encrypted_to_someone_else_is_not_ours() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        d.fee_inner = field(&f.not_ours.inner);
        d.fee_aux = aux_json(&f.not_ours.aux);
        assert_eq!(assess(&r, &d).expect("readable"), FeeNote::NotOurs);
    }

    /// The wallet keeps a note only when its clue is the one its seed yields, so
    /// a leaf published under another clue point is not one this relayer finds.
    #[test]
    fn test_assess_a_note_whose_clue_is_not_its_seeds_is_not_ours() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        let mut aux = f.deposit_fee.aux.clone();
        aux.clue_r = f.fee.aux.clue_r.clone();
        d.fee_aux = aux_json(&aux);
        assert_eq!(assess(&r, &d).expect("readable"), FeeNote::NotOurs);
    }

    /// Its plaintext names another asset than the escrowed `feeAssetId`, so the
    /// leaf lands but does not open as the note the plaintext describes.
    #[test]
    fn test_assess_a_note_naming_another_asset_than_the_fee_asset_is_malformed() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        d.fee_asset_id += 1;
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Malformed("fee note names a different asset")
        );
    }

    /// The fee is paid in another asset than the deposit: the note is checked
    /// against the escrowed `feeAssetId`, not `publicAssetId`.
    #[test]
    fn test_assess_a_cross_asset_fee_note_returns_paid() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        d.public_asset_id = f.asset_id + 1;
        assert_eq!(d.fee_asset_id, f.asset_id);
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Paid { paid: 250 }
        );
    }

    /// A zero-fee escrow names fee asset 0 and pays nothing in any asset, so the
    /// asset check is skipped. The fixture's note is valued, so the value check
    /// is what fails; an asset check would have failed first.
    #[test]
    fn test_assess_a_zero_fee_escrow_does_not_check_the_note_asset() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        d.fee_in = 0;
        d.fee_asset_id = 0;
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Malformed("fee note value disagrees with feeIn")
        );
    }

    /// `feeIn` is what the contract escrowed and what the batch circuit builds
    /// the leaf from, so a plaintext that disagrees describes another leaf.
    #[test]
    fn test_assess_a_note_whose_value_disagrees_with_fee_in_is_malformed() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        d.fee_in += 1;
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Malformed("fee note value disagrees with feeIn")
        );
    }

    /// A pipeline fault rather than a fee outcome: the column is unreadable, so
    /// the caller draws no conclusion about the deposit.
    #[test]
    fn test_assess_an_unreadable_fee_aux_is_an_error() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f);
        d.fee_aux = json!({ "ephPubX": "1" });
        assert!(assess(&r, &d).is_err());
    }
}
