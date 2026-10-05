//! `tests/vectors/shielded-fee.json`, shared by this module's tests.
//!
//! Built by the SDK's own aux and deposit builders; see the generator note in
//! `crates/crypto/src/note/tests.rs`. Every ciphertext here is one a real wallet
//! would produce for these keys.

use super::FeeRecipient;
use super::recipient::point_of;
use crate::adapters::parse::{FieldRef, parse_field};
use crate::domain::dto::OutputAuxDto;
use crypto::clue::pack;
use crypto::note::{self, NotePlaintext};
use crypto::tree::Field;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Fixture {
    pub(super) address: String,
    pub(super) ivk_hex: String,
    pub(super) nullifier0: String,
    pub(super) asset_id: u64,
    /// A slot no key can open.
    pub(super) pad: PointAndCiphertext,
    /// A correct fee note: owned by the relayer, sent to the relayer.
    pub(super) fee: Slot,
    /// A second fee note in the same asset, in another slot.
    pub(super) fee_second: Slot,
    /// A fee note in a different asset.
    pub(super) fee_other_asset: Slot,
    /// Encrypted to the relayer but owned by another party: it decrypts and
    /// its commitment does not match.
    pub(super) foreign_owner: Slot,
    /// Encrypted to another party: must not decrypt.
    pub(super) not_ours: Slot,
    /// The fee leaf of a deposit, from the SDK's `buildDeposit`.
    pub(super) deposit_fee: DepositSlot,
    /// A fee note to another address of the relayer's key.
    pub(super) other_address: Slot,
    /// A fee note that opens its commitment, published under an ephemeral key
    /// that is not its seed's.
    pub(super) wrong_ephemeral: Slot,
}

#[derive(Debug, Deserialize)]
pub(super) struct PointAndCiphertext {
    pub(super) x: String,
    pub(super) y: String,
    pub(super) ct: String,
}

/// A note built for one output slot of a spend.
#[derive(Debug, Deserialize)]
pub(super) struct Slot {
    /// Which output slot this note was built for. `rho` is pinned to it, so a
    /// note is valid only in the slot it names.
    pub(super) index: usize,
    /// The leaf: `cm` over the slot's asset, value and `inner`.
    pub(super) cm: String,
    pub(super) inner: String,
    pub(super) aux: OutputAuxDto,
}

/// A deposit's fee note. Its `rho` is free, so it names no output slot.
#[derive(Debug, Deserialize)]
pub(super) struct DepositSlot {
    pub(super) cm: String,
    pub(super) inner: String,
    pub(super) aux: OutputAuxDto,
}

pub(super) fn fixture() -> Fixture {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/shielded-fee.json"
    )))
    .expect("shielded-fee.json parses")
}

impl Fixture {
    /// The relayer's viewing key.
    pub(super) fn ivk(&self) -> Field {
        parse_field(&self.ivk_hex, FieldRef::Named("ivk"))
            .expect("ivk parses")
            .0
    }

    /// The plaintext of `aux` if the relayer's key decrypts it. Read without
    /// `FeeRecipient::open`, so a test's expectation is not taken from the code
    /// it checks.
    pub(super) fn plaintext_of(&self, aux: &OutputAuxDto) -> Option<NotePlaintext> {
        let wire = hex::decode(aux.ciphertext.trim_start_matches("0x")).expect("hex");
        let (_, body) = note::split_clue_prefix(&wire).expect("clue prefix");
        let epk = point_of(&aux.eph_pub.x, &aux.eph_pub.y, "ephPub", 0).expect("epk parses");
        let raw = note::try_decrypt(&self.ivk(), &pack(&epk), body)?;
        Some(NotePlaintext::decode(&raw).expect("plaintext decodes"))
    }
}

/// The relayer the fixture's fee notes pay.
pub(super) fn recipient(f: &Fixture) -> FeeRecipient {
    FeeRecipient::new(f.address.clone(), f.ivk()).expect("address and key agree")
}
