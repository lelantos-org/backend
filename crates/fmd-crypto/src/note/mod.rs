//! Note plaintexts, key derivations, and commitments.
//!
//! The half of the note format a *recipient* needs: enough to recognise an
//! output as one's own, and to check that what the ciphertext claims is what
//! the proof committed to. Building notes is the wallet's job and lives in the
//! SDK; nothing here constructs one.
//!
//! Key hierarchy, mirroring `circuits/src/lib/note.circom` and
//! `sdk/src/crypto/derive.ts`:
//!
//! ```text
//! nsk -> ivk = Poseidon(TAG_IVK, nsk) -> pk   = Poseidon(TAG_PK, ivk)
//!                                     -> pk_d = (ivk mod q)·B8
//!     -> nk  = Poseidon(TAG_NK, nsk)
//! ```
//!
//! `ivk` is the whole of what this module needs. It recovers `pk`, and with it
//! the ability to recognise a note, but not `nsk`, and so not the ability to
//! spend one — which is what lets a service verify payments to an address whose
//! spend authority is held somewhere else entirely.
//!
//! Field elements cross this module's boundary as [`Field`]: big-endian 32
//! bytes, the same convention `tree` uses. The little-endian spellings the wire
//! format uses do not escape.

mod decrypt;
#[cfg(test)]
mod tests;

pub use decrypt::try_decrypt;

use crate::poseidon::{self, PoseidonError};
use crate::tree::{Field, be_to_fq, fq_to_be};
use ark_ed_on_bn254::Fq;
use ark_ff::PrimeField;

/// Domain-separation tags. Mirror `circuits/src/lib/tags.circom`; the values
/// are consensus, and changing one invalidates every issued proof.
pub const TAG_PK: u64 = 3;
pub const TAG_RHO: u64 = 11;

/// `asset_id` and `value` are packed into one field element as
/// `asset_id · 2^64 + value`, which is why the circuit range-checks both to 64
/// bits.
const POW_2_64: u128 = 1 << 64;

/// Plaintext length: `asset(8) || value(8) || rho(32) || rcm(32) || rcv_dep(32)`,
/// every field little-endian. Mirrors `NOTE_PLAINTEXT_BYTES` in
/// `sdk/src/notes/codec.ts`.
pub const NOTE_PLAINTEXT_BYTES: usize = 112;

/// The wire ciphertext carries the FMD clue bits in front of the AEAD body, as
/// two big-endian bytes. `PubInputs.sol` reads those same two bytes to
/// recompute the `clueBits` public input, so they belong to the proof rather
/// than to the ciphertext.
pub const CLUE_BITS_PREFIX_BYTES: usize = 2;

/// What the recipient learns from a note they can decrypt.
///
/// `pk` is absent because the recipient derives it from their own `ivk`, and
/// `rcv` because it is chosen per spend rather than transmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotePlaintext {
    pub asset_id: u64,
    pub value: u64,
    pub rho: Field,
    pub rcm: Field,
    pub rcv_dep: Field,
}

impl NotePlaintext {
    /// Parse the fixed 112-byte layout. `None` on any other length.
    ///
    /// A non-canonical field element is reduced rather than rejected. Nothing
    /// is trusted on the strength of this parse — a caller must still rebuild
    /// the commitment and match it against the one the proof carried, and a
    /// reduced element simply fails that check.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() != NOTE_PLAINTEXT_BYTES {
            return None;
        }
        let le_field =
            |range: std::ops::Range<usize>| fq_to_be(Fq::from_le_bytes_mod_order(&buf[range]));
        Some(Self {
            asset_id: u64::from_le_bytes(buf[0..8].try_into().ok()?),
            value: u64::from_le_bytes(buf[8..16].try_into().ok()?),
            rho: le_field(16..48),
            rcm: le_field(48..80),
            rcv_dep: le_field(80..112),
        })
    }
}

/// Split the two-byte clue prefix off a wire ciphertext, yielding the AEAD
/// body. `None` if the ciphertext is too short to carry one.
pub fn strip_clue_prefix(wire: &[u8]) -> Option<&[u8]> {
    wire.get(CLUE_BITS_PREFIX_BYTES..)
}

/// `pk = Poseidon(TAG_PK, ivk)` — the note-commitment binding key. Public: it
/// travels in the payment address so any sender can build a commitment for the
/// recipient.
pub fn derive_pk(ivk: &Field) -> Result<Field, PoseidonError> {
    hash_to_field(&[Fq::from(TAG_PK), be_to_fq(ivk)])
}

/// `rho = Poseidon(TAG_RHO, nullifier[0], index)` for output note `index`.
///
/// The circuit pins every output's `rho` to this, so a verifier recomputes it
/// from public inputs rather than taking the sender's word for it.
pub fn derive_rho(nullifier0: &Field, index: u64) -> Result<Field, PoseidonError> {
    hash_to_field(&[Fq::from(TAG_RHO), be_to_fq(nullifier0), Fq::from(index)])
}

/// `cm = Poseidon(asset_id·2^64 + value, pk, rho, rcm)`.
///
/// No leading tag: `asset_id != 0` is enforced in circuit, so the packed first
/// element is at least `2^64` and cannot collide with a preimage whose first
/// element is one of the small constant tags.
pub fn commitment(
    asset_id: u64,
    value: u64,
    pk: &Field,
    rho: &Field,
    rcm: &Field,
) -> Result<Field, PoseidonError> {
    let packed = Fq::from(asset_id) * Fq::from(POW_2_64) + Fq::from(value);
    hash_to_field(&[packed, be_to_fq(pk), be_to_fq(rho), be_to_fq(rcm)])
}

fn hash_to_field(inputs: &[Fq]) -> Result<Field, PoseidonError> {
    poseidon::hash(inputs).map(fq_to_be)
}
