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
use crate::app::config::{BPS_DENOMINATOR, ShieldedFeeSettings};
use crate::domain::dto::{OutputAuxDto, PointDto, PubInputsDto, TRANSACT_OUT};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{ShieldedFeeOut, TokenOut};
use crate::domain::units::{Rate, Scale};
use crate::repositories::assets::AssetRow;
use crate::services::asset_registry::AssetRegistry;
use crate::services::fee_quote::{FeeQuoter, FeeToken};
use alloy::primitives::U256;
use fmd_crypto::clue::{fq_from_be_bytes, pack, point_from_xy};
use fmd_crypto::note::{self, NotePlaintext};
use fmd_crypto::tree::Field;
use std::fmt;
use std::sync::Arc;
use tracing::info;

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

/// A fee the relayer accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaidFee {
    pub asset_id: u64,
    /// `circuit_total * scale`: what the payment is worth in base units.
    pub base_amount: U256,
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

/// See [`FeeRecipient`]'s `Debug`: this reaches the key through `recipient`.
impl fmt::Debug for ShieldedFeeChecker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShieldedFeeChecker")
            .field("chain_id", &self.chain_id)
            .field("recipient", &self.recipient)
            .field("grace_bps", &self.grace_bps)
            .field("allowlist", &self.policy.allowlist)
            .finish_non_exhaustive()
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
        let fx = fq_from_be_bytes(&field_of(x, FieldRef::Coord("fee_aux.ephPub", 0, "x"))?);
        let fy = fq_from_be_bytes(&field_of(y, FieldRef::Coord("fee_aux.ephPub", 0, "y"))?);
        Ok(pack(&point_from_xy(fx, fy)))
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
        let epk = pack_point(&slot.eph_pub, index)?;
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

/// Which assets this relayer will take as a fee, and what they are worth.
///
/// Two gates, combined with AND: the operator's explicit allowlist, and whether
/// `accepted_fee_tokens` can price the token. An empty allowlist reads as
/// everything, but an asset the fee table cannot price has no quote to check a
/// payment against.
///
/// One type, because `/chains` and the submit path ask the same question and an
/// answer differing between them would advertise a fee that then 402s after the
/// wallet has built and proved the spend.
struct FeePolicy {
    /// Empty means no explicit restriction rather than everything; the fee table
    /// still decides.
    allowlist: Vec<u64>,
    fee_quoter: Arc<FeeQuoter>,
}

impl FeePolicy {
    fn allowlists(&self, asset_id: u64) -> bool {
        self.allowlist.is_empty() || self.allowlist.contains(&asset_id)
    }

    /// The fee-table entry that prices `asset`, if there is one.
    ///
    /// The registry keys on a MASP asset id and the fee table keys on an ERC-20
    /// address, so the token address is the join between them.
    fn price_for(&self, asset: &AssetRow) -> Option<&FeeToken> {
        self.fee_quoter.token_at(&asset.token_address()?)
    }

    /// Whether a registered asset can actually pay a fee.
    fn accepts(&self, asset: &AssetRow) -> bool {
        self.allowlists(asset.asset_id()) && self.price_for(asset).is_some()
    }

    /// The subset of `registered` a payer may use. What `/chains` publishes.
    fn payable<'a>(&'a self, registered: &'a [AssetRow]) -> impl Iterator<Item = &'a AssetRow> {
        registered.iter().filter(|a| self.accepts(a))
    }
}

/// Recognition plus pricing: what a chain needs to actually charge.
pub struct ShieldedFeeChecker {
    chain_id: i64,
    recipient: FeeRecipient,
    grace_bps: u32,
    policy: FeePolicy,
    assets: Arc<AssetRegistry>,
}

impl ShieldedFeeChecker {
    /// Build a checker for one chain.
    ///
    /// Fails when the configured address and viewing key do not describe the
    /// same identity — see [`FeeRecipient::new`].
    pub fn new(
        chain_id: i64,
        settings: ShieldedFeeSettings<'_>,
        ivk: Field,
        fee_quoter: Arc<FeeQuoter>,
        assets: Arc<AssetRegistry>,
    ) -> AppResult<Self> {
        let recipient = FeeRecipient::new(settings.address.to_string(), ivk)
            .map_err(|e| AppError::Internal(format!("chain {chain_id}: {e}")))?;
        Ok(Self {
            chain_id,
            recipient,
            grace_bps: settings.grace_bps,
            policy: FeePolicy {
                allowlist: settings.assets.to_vec(),
                fee_quoter,
            },
            assets,
        })
    }

    pub fn address(&self) -> &str {
        self.recipient.address()
    }

    /// The identity to trial-decrypt with, for the deposit fee leaf.
    pub fn recipient(&self) -> &FeeRecipient {
        &self.recipient
    }

    /// What one deposit must pay to be worth flushing, in circuit units of
    /// `asset_id`.
    ///
    /// Rounded up to a whole circuit unit and reduced by the configured grace,
    /// mirroring the spend path: a note value is a whole unit, so rounding down
    /// would set a bar no payer could hit exactly.
    pub async fn deposit_fee_required(&self, asset_id: u64, gas_used: u64) -> AppResult<u64> {
        let (rate, fee_token) = self.priced_asset(asset_id).await?;
        let required = self
            .policy
            .fee_quoter
            .required_amount(&fee_token, gas_used)
            .await?;
        let circuit = rate.to_circuit_ceil(apply_grace(required, self.grace_bps));
        // A required amount past 64 bits cannot be paid: the circuit range-checks
        // a note's value to 64 bits. Saturating rather than erroring keeps this a
        // fee decision, leaving every deposit short, instead of aborting the tick
        // for the rest of the batch.
        Ok(u64::try_from(circuit).unwrap_or(u64::MAX))
    }

    /// The terms `/chains` publishes.
    ///
    /// Only assets the indexer has registered and [`FeePolicy`] accepts appear. An
    /// unregistered one carries no `scale`, without which a client cannot turn a
    /// quoted amount into a note value, and an unpriced one would be refused on
    /// submission.
    pub fn terms(&self, registered: &[AssetRow]) -> ShieldedFeeOut {
        ShieldedFeeOut {
            address: self.address().to_string(),
            grace_bps: self.grace_bps,
            markup_bps: self.policy.fee_quoter.markup_bps,
            tokens: self
                .policy
                .payable(registered)
                .map(TokenOut::from)
                .collect(),
        }
    }

    /// Refuse the submission unless its outputs pay this relayer enough to
    /// cover `gas_used`.
    ///
    /// Callers source `gas_used` from the gas witness, exactly as
    /// `FeeQuoter::quote_for_gas` does, so the fee a payer is held to is the
    /// one `/v1/spend/estimate` would have quoted them.
    ///
    /// Called after the transact proof is verified and before the tree-mirror lock
    /// is taken, so a refusal costs neither a Groth16 nor another submission's
    /// turn.
    pub async fn require(
        &self,
        pi: &PubInputsDto,
        aux: &[OutputAuxDto; TRANSACT_OUT],
        gas_used: u64,
    ) -> AppResult<PaidFee> {
        let Some(payment) = self.recipient.find_payment(pi, aux)? else {
            return Err(AppError::ShieldedFeeMissing {
                address: self.address().to_string(),
            });
        };
        let (rate, fee_token) = self.priced_asset(payment.asset_id).await?;
        let paid = rate.to_base(payment.circuit_total);
        let required = self
            .policy
            .fee_quoter
            .required_amount(&fee_token, gas_used)
            .await?;
        let floor = apply_grace(required, self.grace_bps);

        if paid < floor {
            return Err(AppError::ShieldedFeeTooLow {
                asset_id: payment.asset_id,
                required: required.to_string(),
                paid: paid.to_string(),
                grace_bps: self.grace_bps,
            });
        }

        // Both sides of the decision, at the moment it is made. A refusal already
        // carries these in `ShieldedFeeTooLow`; logging an accepted fee records
        // whether relaying is paying for itself without reproducing the quote from
        // the gas figure logged separately.
        //
        // `paid` and `required` are base units of `asset_id`, and `circuit_total`
        // is what the payer's note carried. None of it identifies the payer, who
        // is not linked to the note even by this relayer.
        info!(
            chain_id = self.chain_id,
            asset_id = payment.asset_id,
            paid = %paid,
            required = %required,
            floor = %floor,
            grace_bps = self.grace_bps,
            circuit_total = payment.circuit_total,
            gas_used,
            "shielded fee accepted"
        );
        Ok(PaidFee {
            asset_id: payment.asset_id,
            base_amount: paid,
        })
    }

    /// Resolve an asset to the two things pricing needs: its unit scale and the
    /// fee-table entry that gives it a price.
    ///
    /// Every failure here gives the payer the same answer, that this relayer will
    /// not take that asset, so they share one error. Which reason applies is an
    /// operator concern and appears in the message.
    /// The asset's rate and its fee token.
    ///
    /// Returns a [`Rate`] rather than a [`Scale`] because a yield asset's unit
    /// is worth `gross / supply`, not `scale`. Pricing one at `scale` demands
    /// more units than the gas costs and then values the payment at less than
    /// the pool would hand over, so a wallet that converts correctly is refused
    /// as underpaid.
    async fn priced_asset(&self, asset_id: u64) -> AppResult<(Rate, FeeToken)> {
        if !self.policy.allowlists(asset_id) {
            return Err(self.unaccepted(asset_id, "it is not in shielded_fee_assets"));
        }
        let asset = self
            .assets
            .by_asset_id(self.chain_id, asset_id)
            .await?
            .ok_or_else(|| self.unaccepted(asset_id, "it is not a registered asset"))?;
        let scale = Scale::from_decimal(&asset.scale).ok_or_else(|| {
            AppError::Internal(format!(
                "chain {}: asset {asset_id} has an unusable scale {}",
                self.chain_id, asset.scale
            ))
        })?;
        let rate = asset.rate(scale).ok_or_else(|| {
            self.unaccepted(
                asset_id,
                "it is a yield asset whose index has not been indexed yet",
            )
        })?;
        let fee_token = self
            .policy
            .price_for(&asset)
            .ok_or_else(|| {
                self.unaccepted(
                    asset_id,
                    "it is not in this relayer's accepted_fee_tokens, so it has no price",
                )
            })?
            .clone();
        Ok((rate, fee_token))
    }

    fn unaccepted(&self, asset_id: u64, why: &str) -> AppError {
        AppError::ShieldedFeeAssetRejected {
            asset_id,
            reason: why.to_string(),
        }
    }
}

/// The floor a payment must clear: the quote less the grace band.
///
/// `grace_bps` is bounded below `BPS_DENOMINATOR` by config validation, so this
/// cannot reduce the floor to nothing.
fn apply_grace(required: U256, grace_bps: u32) -> U256 {
    required * U256::from(BPS_DENOMINATOR - grace_bps) / U256::from(BPS_DENOMINATOR)
}

/// Compress an `(x, y)` pair back to the 32 wire bytes the note KDF hashes.
///
/// The payload carries coordinates because the contract needs them, while the KDF
/// was keyed over the packed form the wallet sent. Packing is canonical, so this
/// recovers exactly those bytes.
fn pack_point(p: &PointDto, index: usize) -> AppResult<[u8; 32]> {
    let x = fq_from_be_bytes(&field_of(&p.x, FieldRef::Coord("aux.ephPub", index, "x"))?);
    let y = fq_from_be_bytes(&field_of(&p.y, FieldRef::Coord("aux.ephPub", index, "y"))?);
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
    /// `crates/fmd-crypto/src/note/tests.rs`. Every ciphertext here is one a real
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
        serde_json::from_str(include_str!("../../tests/vectors/shielded-fee.json"))
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

    /// A plain asset: no venue, so it prices at `scale` forever.
    fn row(asset_id: i64, token: u8) -> AssetRow {
        AssetRow {
            asset_id_u64: asset_id,
            token: vec![token; 20],
            scale: "1000000000000".parse().expect("scale"),
            decimals: Some(18),
            symbol: None,
            deposit_bps: None,
            withdraw_bps: None,
            venue: None,
            gross: None,
            total_normalized: None,
            accrued_fee_normalized: None,
            halted: None,
            index_ray: None,
        }
    }

    /// Both directions of the mispricing a yield asset used to cause.
    ///
    /// Before `Rate`, this path converted with `scale` alone: it demanded
    /// `required / scale` units where `required / (scale * index)` covers the
    /// cost, and then valued a payment at `paid * scale` instead of
    /// `paid * scale * index`. The two compound — the relayer asks for more than
    /// it needs and credits less than it was given — so a wallet converting
    /// correctly is refused as underpaid, while one repeating the same stale
    /// error appears to work.
    #[test]
    fn a_yield_asset_prices_off_its_index_not_its_scale() {
        let scale = Scale::from_decimal(&row(1, 1).scale).expect("scale");
        let plain = row(1, 1).rate(scale).expect("a plain asset always prices");
        let earning = yield_row(2, 2)
            .rate(scale)
            .expect("a polled yield asset prices");

        // Covering a fixed gas cost takes fewer units than `scale` demands.
        let cost = U256::from(1_100_000_000_000_000u64);
        assert!(
            earning.to_circuit_ceil(cost) < plain.to_circuit_ceil(cost),
            "pricing at scale over-demands units"
        );

        // And the same units are worth more than `scale` credits.
        assert!(
            earning.to_base(1_000) > plain.to_base(1_000),
            "pricing at scale under-credits the payment"
        );
    }

    /// A yield asset the poller has not reached yet cannot be priced.
    ///
    /// `scale` is not a conservative fallback here — it is wrong by whatever the
    /// venue has already earned — so the row yields no rate and the caller
    /// declines rather than quoting.
    #[test]
    fn a_yield_asset_without_an_index_refuses_to_price() {
        let scale = Scale::from_decimal(&row(1, 1).scale).expect("scale");
        let mut unpolled = yield_row(2, 2);
        unpolled.gross = None;
        assert!(unpolled.rate(scale).is_none());
    }

    /// A yield asset whose venue has earned, so a unit is worth more than
    /// `scale`. `gross / supply` is `1.1 * scale`.
    fn yield_row(asset_id: i64, token: u8) -> AssetRow {
        AssetRow {
            venue: Some(vec![0xaa; 20]),
            gross: Some("1100000000000000".parse().expect("gross")),
            total_normalized: Some("1000".parse().expect("supply")),
            accrued_fee_normalized: Some("0".parse().expect("fee")),
            halted: Some(false),
            index_ray: Some("1100000000000000000000000000".parse().expect("index")),
            ..row(asset_id, token)
        }
    }

    fn fee_token(token: u8) -> crate::app::config::FeeTokenCfg {
        crate::app::config::FeeTokenCfg {
            symbol: format!("T{token}"),
            address: format!("0x{}", hex::encode([token; 20])),
            decimals: 18,
            quote_symbol: "USDC".to_string(),
        }
    }

    /// A policy over a real `FeeQuoter`, so these tests exercise the shipped rule
    /// rather than a stand-in.
    ///
    /// Neither the oracle nor the RPC endpoint is contacted: `token_at` reads the
    /// configured table and `RpcEndpoint::new` only parses a URL.
    fn policy(allowlist: &[u64], priced: &[u8]) -> FeePolicy {
        struct NoOracle;
        #[async_trait::async_trait]
        impl crate::services::oracle::PriceOracle for NoOracle {
            async fn price(&self, _: &str, _: &str) -> AppResult<f64> {
                unreachable!("the fee table is read directly; no pricing happens here")
            }
        }
        FeePolicy {
            allowlist: allowlist.to_vec(),
            fee_quoter: Arc::new(FeeQuoter {
                chain_id: 31337,
                native_symbol: "ETH".to_string(),
                native_decimals: 18,
                accepted_fee_tokens: priced
                    .iter()
                    .map(|t| FeeToken::from_cfg(&fee_token(*t)).expect("fee token"))
                    .collect(),
                oracle: Arc::new(NoOracle),
                gas_estimator: Arc::new(crate::services::gas_estimator::GasEstimator::new(
                    31337,
                    crate::adapters::rpc::RpcEndpoint::new("http://127.0.0.1:1").expect("url"),
                )),
                markup_bps: 1000,
            }),
        }
    }

    fn payable_ids(policy: &FeePolicy, registered: &[AssetRow]) -> Vec<u64> {
        policy.payable(registered).map(AssetRow::asset_id).collect()
    }

    /// An empty allowlist reads as everything, but the fee table must still be
    /// able to price the asset.
    ///
    /// An operator who lists WETH and USDC in `accepted_fee_tokens` and leaves
    /// `shielded_fee_assets` unset must not see DAI advertised in `/chains`, or a
    /// wallet would build and prove a whole spend before the submit refused it.
    #[test]
    fn an_asset_the_fee_table_cannot_price_is_not_advertised() {
        let registered = [row(1, 0xaa), row(2, 0xbb), row(3, 0xcc)];
        let ids = payable_ids(&policy(&[], &[0xaa, 0xbb]), &registered);
        assert_eq!(ids, vec![1, 2], "asset 3 has no price and must not appear");
    }

    #[test]
    fn the_allowlist_narrows_the_priced_set_further() {
        let registered = [row(1, 0xaa), row(2, 0xbb)];
        assert_eq!(
            payable_ids(&policy(&[2], &[0xaa, 0xbb]), &registered),
            vec![2]
        );
    }

    /// Both gates are combined with AND: allowlisting an asset does not supply a
    /// price.
    #[test]
    fn allowlisting_an_unpriced_asset_does_not_advertise_it() {
        let registered = [row(3, 0xcc)];
        assert!(payable_ids(&policy(&[3], &[0xaa]), &registered).is_empty());
    }

    /// A row whose address column is not 20 bytes must drop out rather than panic;
    /// the column is written by another service.
    #[test]
    fn a_row_with_a_malformed_token_address_is_skipped() {
        let mut bad = row(1, 0xaa);
        bad.token = vec![0xaa; 19];
        assert!(bad.token_address().is_none());
        assert!(payable_ids(&policy(&[], &[0xaa]), &[bad]).is_empty());
    }

    #[test]
    fn the_grace_band_lowers_the_floor_by_exactly_its_share() {
        let required = U256::from(10_000u64);
        assert_eq!(apply_grace(required, 0), required);
        assert_eq!(apply_grace(required, 300), U256::from(9_700u64));
        assert_eq!(apply_grace(required, 5_000), U256::from(5_000u64));
    }
}
