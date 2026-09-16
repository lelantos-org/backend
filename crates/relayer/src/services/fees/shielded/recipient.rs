//! Verifying that a submission paid the relayer, without learning who paid.
//!
//! The payer funds one of the transact circuit's output slots with a note
//! addressed to the relayer's shielded address. That note travels in the payload
//! the relayer already receives, since `aux[j]` carries the sender's ephemeral
//! key and the encrypted note, so collecting a fee costs no extra round trip, no
//! extra calldata and no on-chain transfer linking the payer to the spend.
//!
//! # What makes the amount trustworthy
//!
//! A ciphertext asserts whatever its author chose, so three checks are needed to
//! make it actionable:
//!
//! 1. The proof is verified first. `verify_transact_proof` runs before this, so
//!    `out_cm` and `nullifier[0]` are values a valid SNARK committed to rather
//!    than caller-supplied.
//! 2. `out_aux_digest` binds the ciphertext. The final coefficient of the
//!    Fiat-Shamir compression covers every `aux` entry, so the ciphertext this
//!    module decrypts is the one the prover committed to; nobody, this relayer
//!    included, can swap it and keep the proof valid.
//! 3. The commitment is rebuilt. `cm = Poseidon(asset·2^64 + value, pk, rho,
//!    rcm)` is recomputed from the decrypted plaintext against the relayer's own
//!    `pk` and must equal `out_cm[j]`. A note encrypted to us but owned by
//!    another party fails this, as does one whose plaintext inflates the value.
//!
//! Only `ivk` is required, so the spending key that could move collected fees
//! never exists on this host.

use crate::adapters::parse::{FieldRef, parse_field, parse_hex_bytes};
use crate::domain::dto::{OutputAuxDto, PubInputsDto, TRANSACT_OUT};
use crate::domain::error::{AppError, AppResult};
use crypto::clue::{fq_from_be_bytes, pack, point_from_xy};
use crypto::note::{self, NotePlaintext};
use crypto::tree::Field;
use std::fmt;

/// What one submission paid, before it is priced.
///
/// `circuit_total` is a `u128` because it is a sum: each note's value is bounded
/// by the circuit's 64-bit range check and there are `TRANSACT_OUT` of them, so
/// the total does not fit `u64`. Widening removes an otherwise unreachable
/// overflow branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payment {
    pub asset_id: u64,
    pub circuit_total: u128,
}

/// Recognises notes addressed to one shielded identity.
///
/// Pure: no network, database or clock. Everything that makes a decrypted value
/// trustworthy lives here, so it can be tested against real wallet-produced
/// payloads rather than a mock.
pub struct FeeRecipient {
    /// Big-endian incoming viewing key. Decrypt-only.
    ivk: Field,
    /// `Poseidon(TAG_PK, ivk)`, derived once at boot.
    pk: Field,
    /// The published address, echoed by `/chains`. Never re-derived from `ivk`:
    /// publishing the operator's string verbatim makes a mismatch between the two
    /// a boot failure.
    address: String,
}

/// Hand-written rather than derived: this struct holds a viewing key, and a
/// derived `Debug` would print it into any log line, span field or panic message
/// formatting a value that contains one. The address identifies the recipient and
/// is public.
impl fmt::Debug for FeeRecipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FeeRecipient")
            .field("address", &self.address)
            .field("ivk", &"<redacted>")
            .finish()
    }
}

impl FeeRecipient {
    /// Build a recipient, verifying that the address and the viewing key
    /// describe the same identity.
    ///
    /// The two are configured separately, the address in the TOML and the key
    /// usually from the environment, so they can drift apart. If they do, every
    /// wallet pays an address this relayer cannot decrypt for and every spend is
    /// refused with nothing to point at, so boot fails instead.
    pub fn new(address: String, ivk: Field) -> AppResult<Self> {
        let decoded = crate::domain::shielded_address::decode(&address)?;
        let pk = note::derive_pk(&ivk)
            .map_err(|e| AppError::Internal(format!("shielded fee: derive pk: {e}")))?;
        if pk != decoded.pk {
            return Err(AppError::Internal(format!(
                "shielded_fee_ivk does not belong to shielded_fee_address (the address commits \
                 to pk 0x{}, the key derives 0x{})",
                hex::encode(decoded.pk),
                hex::encode(pk)
            )));
        }
        Ok(Self { ivk, pk, address })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Decrypt-only viewing key, for callers that trial-decrypt a payload this
    /// module does not shape: the deposit fee leaf arrives from the event ledger
    /// rather than from a `PubInputsDto`.
    pub fn ivk(&self) -> &Field {
        &self.ivk
    }

    /// `Poseidon(TAG_PK, ivk)`. Rebuilding a commitment against this separates a
    /// note encrypted to this relayer from one it owns.
    pub fn pk(&self) -> &Field {
        &self.pk
    }

    /// Pack an ephemeral public key given as two decimal-string coordinates,
    /// the form the indexer stores in `deposit_escrowed_events.fee_aux`.
    pub fn pack_epk(&self, x: &str, y: &str) -> AppResult<[u8; 32]> {
        pack_point(x, y, "fee_aux.ephPub", 0)
    }

    /// Everything in this submission that was paid to this recipient.
    ///
    /// Every slot is tried, and one that fails at any step is not ours. A foreign
    /// note, a pad and a malformed one are indistinguishable here by design:
    /// reacting differently to any of them would answer "is this yours?" for
    /// whoever asked.
    ///
    /// Slots are summed rather than first-match, so a payer who splits the fee
    /// across two outputs is credited for both. They must name a single asset: the
    /// circuit permits an output per asset, but a payment spread over several has
    /// no single price to check it against, and `buildSpend` requires every slot
    /// of a spend to share an asset.
    ///
    /// `Err` is reserved for a payload that could not be parsed at all, which is a
    /// shape problem rather than a fee problem.
    pub fn find_payment(
        &self,
        pi: &PubInputsDto,
        aux: &[OutputAuxDto; TRANSACT_OUT],
    ) -> AppResult<Option<Payment>> {
        let nf0 = field_of(&pi.nullifier[0], FieldRef::Index("pubInputs.nullifier", 0))?;
        let mut payment: Option<Payment> = None;

        for (index, slot) in aux.iter().enumerate() {
            let Some(note) = self.decrypt_slot(slot, &nf0, &pi.out_cm[index], index)? else {
                continue;
            };
            match &mut payment {
                None => {
                    payment = Some(Payment {
                        asset_id: note.asset_id,
                        circuit_total: u128::from(note.value),
                    });
                }
                Some(p) if p.asset_id == note.asset_id => {
                    p.circuit_total += u128::from(note.value);
                }
                Some(p) => {
                    return Err(AppError::ShieldedFeeAssetRejected {
                        asset_id: note.asset_id,
                        reason: format!(
                            "the submission also pays in asset {}, and a fee split across assets \
                             has no single price to check it against",
                            p.asset_id
                        ),
                    });
                }
            }
        }
        Ok(payment)
    }

    /// One slot: trial-decrypt, then prove the plaintext is the one the SNARK
    /// committed to.
    fn decrypt_slot(
        &self,
        slot: &OutputAuxDto,
        nf0: &Field,
        out_cm: &str,
        index: usize,
    ) -> AppResult<Option<NotePlaintext>> {
        let wire = parse_hex_bytes(&slot.ciphertext, "aux ciphertext")?;
        let Some(body) = note::strip_clue_prefix(&wire) else {
            return Ok(None);
        };
        let epk = pack_point(&slot.eph_pub.x, &slot.eph_pub.y, "aux.ephPub", index)?;
        let Some(plaintext) = note::try_decrypt(&self.ivk, &epk, body) else {
            return Ok(None);
        };
        let Some(plain) = NotePlaintext::decode(&plaintext) else {
            return Ok(None);
        };

        // `rho` is pinned by the circuit to `Poseidon(TAG_RHO, nf0, index)`, so it
        // is recomputable from public inputs. Checking it is a cheap filter; the
        // commitment below binds the value.
        let rho = note::derive_rho(nf0, index as u64)
            .map_err(|e| AppError::Internal(format!("derive rho: {e}")))?;
        if rho != plain.rho {
            return Ok(None);
        }

        // Rebuilt against this relayer's own `pk`, so a note encrypted to us but
        // owned by another party fails, and rebuilt from the plaintext's own asset
        // and value, so an inflated value fails too.
        let cm = note::commitment(
            plain.asset_id,
            plain.value,
            &self.pk,
            &plain.rho,
            &plain.rcm,
        )
        .map_err(|e| AppError::Internal(format!("note commitment: {e}")))?;
        // Parsed only now: several slots are examined per submission, and all but
        // the paying one have already been discarded.
        if cm != field_of(out_cm, FieldRef::Index("pubInputs.outCm", index))? {
            return Ok(None);
        }
        Ok(Some(plain))
    }
}

/// Compress an `(x, y)` pair back to the 32 wire bytes the note KDF hashes.
///
/// The payload carries coordinates because the contract needs them, while the KDF
/// was keyed over the packed form the wallet sent. Packing is canonical, so this
/// recovers exactly those bytes. `array` and `index` name the point for an error.
fn pack_point(x: &str, y: &str, array: &str, index: usize) -> AppResult<[u8; 32]> {
    let x = fq_from_be_bytes(&field_of(x, FieldRef::Coord(array, index, "x"))?);
    let y = fq_from_be_bytes(&field_of(y, FieldRef::Coord(array, index, "y"))?);
    Ok(pack(&point_from_xy(x, y)))
}

/// A payload field element as big-endian bytes, rejecting anything non-canonical,
/// the same bar `parse_spend_inputs` applies.
fn field_of(s: &str, at: FieldRef<'_>) -> AppResult<Field> {
    Ok(parse_field(s, at)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::dto::PointDto;
    use serde::Deserialize;

    /// Built by the SDK's own encrypt path; see the generator note in
    /// `crates/crypto/src/note/tests.rs`. Every ciphertext here is one a real
    /// wallet would produce for these keys.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        address: String,
        ivk_hex: String,
        nullifier0: String,
        asset_id: u64,
        /// A slot no key can open.
        pad: PointAndCiphertext,
        /// A correct fee note: owned by the relayer, sent to the relayer.
        fee: Slot,
        /// A second fee note in the same asset, in another slot.
        fee_second: Slot,
        /// A fee note in a different asset.
        fee_other_asset: Slot,
        /// Encrypted to the relayer but owned by another party: it decrypts and
        /// its commitment does not match.
        foreign_owner: Slot,
        /// Encrypted to another party: must not decrypt.
        not_ours: Slot,
    }

    #[derive(Debug, Deserialize)]
    struct PointAndCiphertext {
        x: String,
        y: String,
        ct: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Slot {
        /// Which output slot this note was built for. `rho` is pinned to it, so a
        /// note is valid only in the slot it names.
        index: usize,
        cm: String,
        aux: AuxFixture,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AuxFixture {
        clue_r: PointFixture,
        eph_pub: PointFixture,
        ciphertext: String,
    }

    #[derive(Debug, Deserialize)]
    struct PointFixture {
        x: String,
        y: String,
    }

    fn point(x: &str, y: &str) -> PointDto {
        PointDto {
            x: x.to_string(),
            y: y.to_string(),
        }
    }

    fn fixture() -> Fixture {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/vectors/shielded-fee.json"
        )))
        .expect("shielded-fee.json parses")
    }

    fn recipient(f: &Fixture) -> FeeRecipient {
        let ivk = parse_field(&f.ivk_hex, FieldRef::Named("ivk"))
            .expect("ivk parses")
            .0;
        FeeRecipient::new(f.address.clone(), ivk).expect("address and key agree")
    }

    /// A submission whose every slot is a pad, until one is filled in.
    ///
    /// Slots go where the fixture says they belong, so a test cannot place a note
    /// in a slot its `rho` was not built for; that case has its own test below.
    struct Submission {
        aux: [OutputAuxDto; TRANSACT_OUT],
        out_cm: [String; TRANSACT_OUT],
        nullifier0: String,
    }

    impl Submission {
        fn new(f: &Fixture) -> Self {
            let pad = OutputAuxDto {
                clue_r: point(&f.pad.x, &f.pad.y),
                eph_pub: point(&f.pad.x, &f.pad.y),
                ciphertext: f.pad.ct.clone(),
            };
            Self {
                aux: std::array::from_fn(|_| pad.clone()),
                // Distinct placeholders: a commitment colliding with a real one
                // would make a test pass for the wrong reason. Generated over the
                // arity so a wider transact shape keeps them distinct.
                out_cm: std::array::from_fn(|i| (11 + i).to_string()),
                nullifier0: f.nullifier0.clone(),
            }
        }

        fn with(mut self, slot: &Slot) -> Self {
            self.put(slot, slot.index);
            self
        }

        /// Place a note in a slot it was not built for.
        fn with_misplaced(mut self, slot: &Slot, at: usize) -> Self {
            self.put(slot, at);
            self
        }

        fn put(&mut self, slot: &Slot, at: usize) {
            self.aux[at] = OutputAuxDto {
                clue_r: point(&slot.aux.clue_r.x, &slot.aux.clue_r.y),
                eph_pub: point(&slot.aux.eph_pub.x, &slot.aux.eph_pub.y),
                ciphertext: slot.aux.ciphertext.clone(),
            };
            self.out_cm[at] = slot.cm.clone();
        }

        fn find(&self, f: &Fixture) -> AppResult<Option<Payment>> {
            recipient(f).find_payment(&self.pub_inputs(), &self.aux)
        }

        /// Convenience for the cases expecting no payment.
        fn pays_nothing(&self, f: &Fixture) -> bool {
            self.find(f).expect("a well-formed payload").is_none()
        }

        fn pub_inputs(&self) -> PubInputsDto {
            let zero = || point("0", "1");
            PubInputsDto {
                merkle_root: "0".to_string(),
                nullifier: [
                    self.nullifier0.clone(),
                    "2".to_string(),
                    "3".to_string(),
                    "4".to_string(),
                ],
                out_cm: self.out_cm.clone(),
                public_asset_id: 1,
                public_in: 0,
                public_out: 0,
                in_cv: std::array::from_fn(|_| zero()),
                out_cv: std::array::from_fn(|_| zero()),
                out_cv_dep: std::array::from_fn(|_| zero()),
                recipient: "0x0000000000000000000000000000000000000000".to_string(),
                chain_id: 31337,
                payer: "0x0000000000000000000000000000000000000000".to_string(),
                relayer: "0x0000000000000000000000000000000000000000".to_string(),
                intent_hash: "0".to_string(),
            }
        }
    }

    #[test]
    fn recognises_a_correctly_addressed_fee_note() {
        let f = fixture();
        let found = Submission::new(&f)
            .with(&f.fee)
            .find(&f)
            .expect("a well-formed payload")
            .expect("the fee note is ours");
        assert_eq!(found.asset_id, f.asset_id);
        assert_eq!(found.circuit_total, 250);
    }

    /// A payer who splits the fee over two outputs is credited for both.
    #[test]
    fn sums_a_fee_split_across_slots() {
        let f = fixture();
        let found = Submission::new(&f)
            .with(&f.fee)
            .with(&f.fee_second)
            .find(&f)
            .expect("a well-formed payload")
            .expect("both notes are ours");
        assert_eq!(found.circuit_total, 250 + 90);
    }

    /// Two assets have no single price to check a total against, so this is
    /// refused outright rather than half-counted.
    #[test]
    fn refuses_a_fee_split_across_two_assets() {
        let f = fixture();
        let Err(err) = Submission::new(&f)
            .with(&f.fee)
            .with(&f.fee_other_asset)
            .find(&f)
        else {
            panic!("a mixed-asset payment must be refused");
        };
        assert!(
            matches!(err, AppError::ShieldedFeeAssetRejected { .. }),
            "{err}"
        );
    }

    /// Why the commitment is rebuilt: this note decrypts for us, since the sender
    /// used our `pk_d`, but its owner is another party, so it is not a payment.
    #[test]
    fn refuses_a_note_encrypted_to_us_but_owned_by_someone_else() {
        let f = fixture();
        assert!(Submission::new(&f).with(&f.foreign_owner).pays_nothing(&f));
    }

    #[test]
    fn does_not_see_a_note_addressed_to_a_stranger() {
        let f = fixture();
        assert!(Submission::new(&f).with(&f.not_ours).pays_nothing(&f));
    }

    /// An inflated `value` changes the commitment, so the plaintext cannot be the
    /// one the proof committed to.
    #[test]
    fn refuses_a_note_whose_commitment_does_not_match_the_proof() {
        let f = fixture();
        let mut sub = Submission::new(&f).with(&f.fee);
        sub.out_cm[f.fee.index] = "12345".to_string();
        assert!(sub.pays_nothing(&f));
    }

    /// `rho` is pinned to `(nullifier[0], slot)`, so a fee note cannot be
    /// replayed out of the submission it was built for.
    #[test]
    fn refuses_a_fee_note_replayed_under_a_different_nullifier() {
        let f = fixture();
        let mut sub = Submission::new(&f).with(&f.fee);
        sub.nullifier0 = "999999".to_string();
        assert!(sub.pays_nothing(&f));
    }

    /// `rho` is pinned to the slot index too, so the same note is worthless one
    /// slot over.
    #[test]
    fn refuses_a_fee_note_moved_to_another_output_slot() {
        let f = fixture();
        assert!(
            Submission::new(&f)
                .with_misplaced(&f.fee, 0)
                .pays_nothing(&f)
        );
    }

    #[test]
    fn a_submission_of_pads_pays_nothing() {
        let f = fixture();
        assert!(Submission::new(&f).pays_nothing(&f));
    }

    /// An address and a key that do not describe the same identity would make
    /// every wallet pay to somewhere this relayer cannot read.
    #[test]
    fn a_viewing_key_that_does_not_match_its_address_is_refused() {
        let Err(err) = FeeRecipient::new(fixture().address, [7u8; 32]) else {
            panic!("a mismatched key must be refused");
        };
        assert!(err.to_string().contains("does not belong to"), "{err}");
    }

    #[test]
    fn a_malformed_address_is_refused() {
        let ivk = parse_field(&fixture().ivk_hex, FieldRef::Named("ivk"))
            .expect("parses")
            .0;
        assert!(FeeRecipient::new("not-an-address".to_string(), ivk).is_err());
    }

    /// The viewing key must not be reachable through a debug format: it is the one
    /// secret this service holds, and `Debug` is how secrets reach logs.
    #[test]
    fn debug_output_does_not_carry_the_viewing_key() {
        let f = fixture();
        let ivk = parse_field(&f.ivk_hex, FieldRef::Named("ivk"))
            .expect("parses")
            .0;
        let rendered = format!("{:?}", recipient(&f));

        assert!(
            rendered.contains(&f.address),
            "the address should still identify it"
        );
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains(&hex::encode(ivk)));
        // The raw byte array would render as `[44, 183, …]`, which a hex search
        // would not catch.
        assert!(!rendered.contains(&format!("{}", ivk[0])), "{rendered}");
    }
}
