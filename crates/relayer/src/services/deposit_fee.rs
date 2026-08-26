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
//! nobody. Three checks make the leaf actionable:
//!
//! 1. The commitment is rebuilt against this relayer's own `pk`. `feeCm` is
//!    escrow digest preimage, so it is the value the payer signed a Permit2
//!    witness over and neither a relayer nor a flusher can vary it. Rebuilding
//!    `Poseidon(asset·2^64 + value, pk, rho, rcm)` from the decrypted plaintext
//!    fails for a note owned by someone else and for a plaintext that inflates
//!    the value.
//! 2. `rcv_dep` must equal the escrowed `feeRcv`. The batch circuit binds
//!    `cv_dep[k]` to `leaf_public_in[k]` units under blinder `rcv[k]`, and the
//!    relayer supplies that blinder as a witness. A note whose plaintext carries
//!    a different `rcv_dep` decrypts and rebuilds correctly but has no witness,
//!    so the whole batch fails in proving rather than at this deposit. This has
//!    no spend-side analogue.
//! 3. The plaintext must agree with the escrow. `asset_id` and `value` are
//!    checked against the deposit's own `publicAssetId` and `feeIn`. The
//!    contract binds `feeCvDep` to `feeIn` units of the leaf's asset, so a
//!    disagreeing plaintext describes a leaf that cannot be committed.
//!
//! Only `ivk` is required, so the spending key that could move collected fees
//! never exists on this host, as on the spend path.

use crate::domain::error::{AppError, AppResult};
use crate::services::deposit_mempool::PendingDeposit;
use crate::services::shielded_fee::FeeRecipient;
use alloy::primitives::U256;
use fmd_crypto::note::{self, NotePlaintext};
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
    /// Ours, spendable, and worth `paid` circuit units of the deposit's asset.
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
///
/// `clueRx` and `clueRy` are present in the column and not read: FMD narrows a
/// wallet's scan, and this relayer already knows which leaf to try. Serde ignores
/// them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeeAux {
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

    let Some(plain) = decrypt(recipient, &aux)? else {
        return Ok(FeeNote::NotOurs);
    };

    // Rebuilt against this relayer's own `pk` and the plaintext's own asset and
    // value, so a note merely encrypted to us does not pass.
    let cm = note::commitment(
        plain.asset_id,
        plain.value,
        recipient.pk(),
        &plain.rho,
        &plain.rcm,
    )
    .map_err(|e| AppError::Internal(format!("deposit {}: note commitment: {e}", d.id)))?;
    if cm != d.fee_cm {
        return Ok(FeeNote::NotOurs);
    }

    // Past this point the note is provably ours, so a disagreement with the escrow
    // is the payer's fault rather than another party's note.
    if plain.asset_id != d.public_asset_id {
        return Ok(FeeNote::Malformed("fee note names a different asset"));
    }
    if plain.value != d.fee_in {
        return Ok(FeeNote::Malformed("fee note value disagrees with feeIn"));
    }
    // The witness check. Without it the batch fails in proving rather than here.
    if U256::from_be_bytes(plain.rcv_dep) != d.fee_rcv {
        return Ok(FeeNote::Malformed(
            "fee note rcv_dep is not the escrowed feeRcv",
        ));
    }

    Ok(FeeNote::Paid { paid: plain.value })
}

fn decrypt(recipient: &FeeRecipient, aux: &FeeAux) -> AppResult<Option<NotePlaintext>> {
    let wire = hex_bytes(&aux.ciphertext)?;
    let Some(body) = note::strip_clue_prefix(&wire) else {
        return Ok(None);
    };
    let epk = recipient.pack_epk(&aux.eph_pub_x, &aux.eph_pub_y)?;
    let Some(plaintext) = note::try_decrypt(recipient.ivk(), &epk, body) else {
        return Ok(None);
    };
    Ok(NotePlaintext::decode(&plaintext))
}

fn hex_bytes(s: &str) -> AppResult<Vec<u8>> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|e| AppError::Internal(format!("fee_aux ciphertext is not hex: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::parse::{FieldRef, parse_field};
    use fmd_crypto::tree::Field;
    use serde::Deserialize;
    use serde_json::json;

    /// The same vectors the spend path uses: every ciphertext comes from the SDK's
    /// own encrypt path for these keys, so a note here matches what a real wallet
    /// would build.
    ///
    /// The deposit path derives `rho` freely rather than from a nullifier, so only
    /// the fields unaffected by that difference are used: the plaintext, its
    /// commitment and the ephemeral key.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        address: String,
        ivk_hex: String,
        asset_id: u64,
        fee: Slot,
        foreign_owner: Slot,
        not_ours: Slot,
    }

    #[derive(Deserialize)]
    struct Slot {
        cm: String,
        aux: Aux,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Aux {
        clue_r: Point,
        eph_pub: Point,
        ciphertext: String,
    }

    #[derive(Deserialize)]
    struct Point {
        x: String,
        y: String,
    }

    fn fixture() -> Fixture {
        serde_json::from_str(include_str!("../../tests/vectors/shielded-fee.json"))
            .expect("shielded-fee.json parses")
    }

    fn recipient(f: &Fixture) -> FeeRecipient {
        let ivk = parse_field(&f.ivk_hex, FieldRef::Named("ivk"))
            .expect("ivk parses")
            .0;
        FeeRecipient::new(f.address.clone(), ivk).expect("address and key agree")
    }

    fn field(s: &str) -> Field {
        parse_field(s, FieldRef::Named("fixture field"))
            .expect("fixture field parses")
            .0
    }

    fn aux_json(a: &Aux) -> serde_json::Value {
        json!({
            "clueRx": a.clue_r.x,
            "clueRy": a.clue_r.y,
            "ephPubX": a.eph_pub.x,
            "ephPubY": a.eph_pub.y,
            "ciphertext": a.ciphertext,
        })
    }

    /// Decrypt independently of the module under test, so the expected `rcv_dep`
    /// is not taken from the code being checked.
    fn plaintext_of(r: &FeeRecipient, a: &Aux) -> NotePlaintext {
        let wire = hex::decode(a.ciphertext.trim_start_matches("0x")).expect("hex");
        let body = note::strip_clue_prefix(&wire).expect("clue prefix");
        let epk = r.pack_epk(&a.eph_pub.x, &a.eph_pub.y).expect("epk packs");
        let raw = note::try_decrypt(r.ivk(), &epk, body).expect("decrypts for us");
        NotePlaintext::decode(&raw).expect("plaintext decodes")
    }

    /// A deposit whose fee leaf is the fixture's note, escrowed correctly.
    fn deposit_paying(f: &Fixture, r: &FeeRecipient) -> PendingDeposit {
        let plain = plaintext_of(r, &f.fee.aux);
        PendingDeposit {
            id: 1,
            cm: [0xab; 32],
            public_asset_id: f.asset_id,
            public_in: 1_000,
            fee_bps_at_submit: 25,
            payer: [0xcd; 20],
            submitted_at: 99,
            cv_dep: [U256::from(3), U256::from(4)],
            rcv: U256::from(5),
            fee_in: plain.value,
            fee_cm: field(&f.fee.cm),
            fee_cv_dep: [U256::from(6), U256::from(7)],
            fee_rcv: U256::from_be_bytes(plain.rcv_dep),
            fee_aux: aux_json(&f.fee.aux),
        }
    }

    #[test]
    fn test_assess_a_correctly_escrowed_note_returns_paid() {
        let f = fixture();
        let r = recipient(&f);
        let d = deposit_paying(&f, &r);
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Paid { paid: 250 }
        );
    }

    /// This note decrypts for us but its commitment was built against another
    /// party's `pk`, so it is not ours to spend.
    #[test]
    fn test_assess_a_note_owned_by_another_key_is_not_ours() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f, &r);
        d.fee_cm = field(&f.foreign_owner.cm);
        d.fee_aux = aux_json(&f.foreign_owner.aux);
        assert_eq!(assess(&r, &d).expect("readable"), FeeNote::NotOurs);
    }

    /// Encrypted to a different recipient: it does not decrypt, which is
    /// indistinguishable from a pad by design.
    #[test]
    fn test_assess_a_note_encrypted_to_someone_else_is_not_ours() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f, &r);
        d.fee_cm = field(&f.not_ours.cm);
        d.fee_aux = aux_json(&f.not_ours.aux);
        assert_eq!(assess(&r, &d).expect("readable"), FeeNote::NotOurs);
    }

    /// The check with no spend-side analogue. The note is ours and its commitment
    /// rebuilds, so every spend-path check passes, yet the relayer cannot witness
    /// the leaf, which would fail the whole batch in proving rather than here.
    #[test]
    fn test_assess_a_note_whose_rcv_dep_is_not_the_escrowed_fee_rcv_is_malformed() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f, &r);
        d.fee_rcv += U256::from(1u8);
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Malformed("fee note rcv_dep is not the escrowed feeRcv")
        );
    }

    #[test]
    fn test_assess_a_note_naming_another_asset_is_malformed() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f, &r);
        d.public_asset_id += 1;
        assert_eq!(
            assess(&r, &d).expect("readable"),
            FeeNote::Malformed("fee note names a different asset")
        );
    }

    /// `feeIn` is what the contract escrowed and what `feeCvDep` is bound to,
    /// so a plaintext that disagrees describes a leaf that cannot be committed.
    #[test]
    fn test_assess_a_note_whose_value_disagrees_with_fee_in_is_malformed() {
        let f = fixture();
        let r = recipient(&f);
        let mut d = deposit_paying(&f, &r);
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
        let mut d = deposit_paying(&f, &r);
        d.fee_aux = json!({ "ephPubX": "1" });
        assert!(assess(&r, &d).is_err());
    }
}
