//! Note plaintexts, key derivations, and commitments.
//!
//! The recipient's half of the note format: enough to recognise an output as
//! one's own and to check that the ciphertext matches what the proof committed
//! to. Note construction belongs to the wallet and lives in the SDK; nothing here
//! builds a note.
//!
//! Key hierarchy, mirroring `circuits/src/lib/note.circom` and
//! `sdk/src/crypto/derive.ts`:
//!
//! ```text
//! nsk -> ivk = Poseidon(TAG_IVK, nsk) -> pk      = Poseidon(TAG_PK, ivk, d)
//!                                     -> pk_d    = (ivk mod q)·g_d
//!                                     -> dk_root = Poseidon(TAG_DK, ivk) mod q
//!                                     -> ck_d    = dk_root·g_d
//!     -> nk  = Poseidon(TAG_NK, nsk)
//! ```
//!
//! `d` is the 16-byte diversifier of the address the note is held under and
//! `g_d` its base point, a function of `d` alone ([`diversified_base`]). An
//! address publishes `(d, pk_d, pk, ck_d)`; see [`address_keys`].
//!
//! `ivk` is all this module requires. It recovers every key of an address, and
//! with them the ability to recognise a note, but not `nsk`, so it confers no
//! ability to spend. That lets a service verify payments to an address whose
//! spend authority is held elsewhere.
//!
//! Field elements cross this module's boundary as [`Field`]: big-endian 32 bytes,
//! matching `tree`. The wire format's little-endian spellings do not escape.

mod decrypt;
mod diversified;
mod seed;
#[cfg(test)]
mod tests;

pub use decrypt::try_decrypt;
pub use diversified::{AddressKeys, address_keys, diversified_base};
pub use seed::{ExpandedSeed, Published, expand_seed};

use crate::poseidon::{self, PoseidonError};
use crate::tree::{Field, be_to_fq, fq_to_be};
use ark_ed_on_bn254::Fq;
use ark_ff::{BigInteger, PrimeField};

/// Domain-separation tags mirroring `circuits/src/lib/tags.circom`. The values
/// are consensus; changing one invalidates every issued proof.
pub const TAG_CM: u64 = 1;
pub const TAG_PK: u64 = 3;
pub const TAG_RHO: u64 = 11;
pub const TAG_INNER: u64 = 14;

/// Off-circuit tags, mirroring `sdk/src/crypto/tags.ts`.
pub const TAG_DK: u64 = 6;
pub const TAG_GD: u64 = 16;

/// `asset_id` and `value` are packed into one field element as
/// `asset_id · 2^64 + value`, so the circuit range-checks both to 64 bits.
const POW_2_64: u128 = 1 << 64;

/// Byte width of a diversifier.
pub const DIVERSIFIER_BYTES: usize = 16;

/// Byte width of `rseed`.
pub const SEED_BYTES: usize = 32;

const VALUE_OFFSET: usize = 8;
const RHO_OFFSET: usize = VALUE_OFFSET + 8;
const RSEED_OFFSET: usize = RHO_OFFSET + 32;
const D_OFFSET: usize = RSEED_OFFSET + SEED_BYTES;
const MEMO_OFFSET: usize = D_OFFSET + DIVERSIFIER_BYTES;

/// Byte width of the memo field every plaintext ends with: the sender's UTF-8
/// text, zero-padded. Mirrors `MEMO_BYTES` in `sdk/src/notes/codec.ts`.
pub const MEMO_BYTES: usize = 128;

/// Plaintext length for
/// `asset(8) || value(8) || rho(32) || rseed(32) || d(16) || memo(128)`, every
/// integer little-endian: 224. Mirrors `NOTE_PLAINTEXT_BYTES` in
/// `sdk/src/notes/codec.ts`.
pub const NOTE_PLAINTEXT_BYTES: usize = MEMO_OFFSET + MEMO_BYTES;

/// The wire ciphertext carries the FMD clue bits ahead of the AEAD body as two
/// big-endian bytes. `PubInputs.sol` reads the same two bytes to recompute the
/// `clueBits` public input, so they belong to the proof rather than the
/// ciphertext.
pub const CLUE_BITS_PREFIX_BYTES: usize = 2;

/// What the recipient learns from a note they can decrypt.
///
/// `pk` is absent because the recipient derives it from their own `ivk` and `d`.
/// `rcm` is absent because it is expanded from `rseed`; see [`expand_seed`]. The
/// memo is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotePlaintext {
    pub asset_id: u64,
    pub value: u64,
    pub rho: Field,
    /// Seed of the output's randomness.
    pub rseed: [u8; SEED_BYTES],
    /// Diversifier of the address the note is for, below `2^128`.
    pub d: Field,
}

impl NotePlaintext {
    /// Parse the fixed 224-byte layout. `None` on any other length, or when
    /// `rho` is not a canonical field element: the seed expansion hashes its
    /// bytes, and the wallet refuses such a note.
    ///
    /// This parse establishes no trust: the caller must rebuild the commitment
    /// and match it against the one the proof carried.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() != NOTE_PLAINTEXT_BYTES {
            return None;
        }
        Some(Self {
            asset_id: u64::from_le_bytes(buf[..VALUE_OFFSET].try_into().ok()?),
            value: u64::from_le_bytes(buf[VALUE_OFFSET..RHO_OFFSET].try_into().ok()?),
            rho: le_to_canonical_field(&buf[RHO_OFFSET..RSEED_OFFSET])?,
            rseed: buf[RSEED_OFFSET..D_OFFSET].try_into().ok()?,
            d: le_to_field(&buf[D_OFFSET..MEMO_OFFSET]),
        })
    }
}

/// Split a wire ciphertext into its clue bits and AEAD body. `None` if the
/// ciphertext is too short to carry the two-byte prefix.
pub fn split_clue_prefix(wire: &[u8]) -> Option<(u16, &[u8])> {
    let (prefix, body) = wire.split_at_checked(CLUE_BITS_PREFIX_BYTES)?;
    Some((u16::from_be_bytes(prefix.try_into().ok()?), body))
}

/// `pk = Poseidon(TAG_PK, ivk, d)`, the note-commitment binding key of `ivk`
/// under diversifier `d`. Public: it travels in the payment address so any
/// sender can build a commitment for the recipient.
pub fn derive_pk(ivk: &Field, d: &Field) -> Result<Field, PoseidonError> {
    hash_to_field(&[Fq::from(TAG_PK), be_to_fq(ivk), be_to_fq(d)])
}

/// `rho = Poseidon(TAG_RHO, nullifier[0], index)` for output note `index`.
///
/// The circuit pins every output's `rho` to this, so a verifier recomputes it
/// from public inputs rather than trusting the sender.
pub fn derive_rho(nullifier0: &Field, index: u64) -> Result<Field, PoseidonError> {
    hash_to_field(&[Fq::from(TAG_RHO), be_to_fq(nullifier0), Fq::from(index)])
}

/// `inner = Poseidon(TAG_INNER, pk, rho, rcm)`, the owner half of a note. A
/// deposit publishes it beside its public `(asset, value)`.
pub fn inner(pk: &Field, rho: &Field, rcm: &Field) -> Result<Field, PoseidonError> {
    hash_to_field(&[
        Fq::from(TAG_INNER),
        be_to_fq(pk),
        be_to_fq(rho),
        be_to_fq(rcm),
    ])
}

/// `cm = Poseidon(TAG_CM, asset_id·2^64 + value, inner)`, the note commitment
/// and the tree leaf.
///
/// A spend publishes `cm`. A deposit publishes `inner`, and
/// `tree_update_batch` builds the leaf with this hash, so anything rebuilding
/// the tree from `DepositEscrowed` computes a deposit's leaf here.
///
/// A non-canonical `inner` is reduced rather than rejected. The pool cannot
/// flush such a deposit, so its leaf is never inserted.
pub fn commitment_from_inner(
    asset_id: u64,
    value: u64,
    inner: &Field,
) -> Result<Field, PoseidonError> {
    let packed = Fq::from(asset_id) * Fq::from(POW_2_64) + Fq::from(value);
    hash_to_field(&[Fq::from(TAG_CM), packed, be_to_fq(inner)])
}

/// `cm` of a fully opened note: [`commitment_from_inner`] over [`inner`].
pub fn commitment(
    asset_id: u64,
    value: u64,
    pk: &Field,
    rho: &Field,
    rcm: &Field,
) -> Result<Field, PoseidonError> {
    commitment_from_inner(asset_id, value, &inner(pk, rho, rcm)?)
}

/// Little-endian bytes as a field element, reduced.
fn le_to_field(bytes: &[u8]) -> Field {
    fq_to_be(Fq::from_le_bytes_mod_order(bytes))
}

/// Little-endian bytes as a field element, `None` unless below the modulus.
fn le_to_canonical_field(bytes: &[u8]) -> Option<Field> {
    let x = Fq::from_le_bytes_mod_order(bytes);
    (x.into_bigint().to_bytes_le() == bytes).then(|| fq_to_be(x))
}

fn hash_to_field(inputs: &[Fq]) -> Result<Field, PoseidonError> {
    poseidon::hash(inputs).map(fq_to_be)
}
