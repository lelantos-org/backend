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
//! nsk -> ivk = Poseidon(TAG_IVK, nsk) -> pk   = Poseidon(TAG_PK, ivk, d)
//!                                     -> pk_d = (ivk mod q)·B8
//!     -> nk  = Poseidon(TAG_NK, nsk)
//! ```
//!
//! `d` is the diversifier of the address the note is held under. An account's
//! default address uses [`default_diversifier`], a function of `ivk`.
//!
//! `ivk` is all this module requires. It recovers `pk`, and with it the ability
//! to recognise a note, but not `nsk`, so it confers no ability to spend. That
//! lets a service verify payments to an address whose spend authority is held
//! elsewhere.
//!
//! Field elements cross this module's boundary as [`Field`]: big-endian 32 bytes,
//! matching `tree`. The wire format's little-endian spellings do not escape.

mod decrypt;
#[cfg(test)]
mod tests;

pub use decrypt::try_decrypt;

use crate::poseidon::{self, PoseidonError};
use crate::tree::{Field, be_to_fq, fq_to_be};
use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit};
use ark_ed_on_bn254::Fq;
use ark_ff::PrimeField;
use blake2::Blake2b;
use blake2::digest::Digest;
use blake2::digest::consts::U16;

/// Domain-separation tags mirroring `circuits/src/lib/tags.circom`. The values
/// are consensus; changing one invalidates every issued proof.
pub const TAG_CM: u64 = 1;
pub const TAG_PK: u64 = 3;
pub const TAG_RHO: u64 = 11;
pub const TAG_INNER: u64 = 14;

/// Mirrors `DVK_DOMAIN` in `sdk/src/keys/diversifier.ts`.
const DVK_DOMAIN: &[u8] = b"lelantos.addr.dvk.v1";

/// `asset_id` and `value` are packed into one field element as
/// `asset_id · 2^64 + value`, so the circuit range-checks both to 64 bits.
const POW_2_64: u128 = 1 << 64;

/// Plaintext length for `asset(8) || value(8) || rho(32) || rcm(32)`, every
/// field little-endian. Mirrors `NOTE_PLAINTEXT_BYTES` in
/// `sdk/src/notes/codec.ts`.
pub const NOTE_PLAINTEXT_BYTES: usize = 80;

/// The wire ciphertext carries the FMD clue bits ahead of the AEAD body as two
/// big-endian bytes. `PubInputs.sol` reads the same two bytes to recompute the
/// `clueBits` public input, so they belong to the proof rather than the
/// ciphertext.
pub const CLUE_BITS_PREFIX_BYTES: usize = 2;

/// What the recipient learns from a note they can decrypt.
///
/// `pk` is absent because the recipient derives it from their own `ivk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotePlaintext {
    pub asset_id: u64,
    pub value: u64,
    pub rho: Field,
    pub rcm: Field,
}

impl NotePlaintext {
    /// Parse the fixed 80-byte layout. `None` on any other length.
    ///
    /// A non-canonical field element is reduced rather than rejected. This parse
    /// establishes no trust: the caller must rebuild the commitment and match it
    /// against the one the proof carried, which a reduced element fails.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() != NOTE_PLAINTEXT_BYTES {
            return None;
        }
        let le_field = |range: std::ops::Range<usize>| le_to_field(&buf[range]);
        Some(Self {
            asset_id: u64::from_le_bytes(buf[0..8].try_into().ok()?),
            value: u64::from_le_bytes(buf[8..16].try_into().ok()?),
            rho: le_field(16..48),
            rcm: le_field(48..80),
        })
    }
}

/// Split the two-byte clue prefix off a wire ciphertext, yielding the AEAD
/// body. `None` if the ciphertext is too short to carry one.
pub fn strip_clue_prefix(wire: &[u8]) -> Option<&[u8]> {
    wire.get(CLUE_BITS_PREFIX_BYTES..)
}

/// `pk = Poseidon(TAG_PK, ivk, d)`, the note-commitment binding key of `ivk`
/// under diversifier `d`. Public: it travels in the payment address so any
/// sender can build a commitment for the recipient.
pub fn derive_pk(ivk: &Field, d: &Field) -> Result<Field, PoseidonError> {
    hash_to_field(&[Fq::from(TAG_PK), be_to_fq(ivk), be_to_fq(d)])
}

/// The diversifier of the account's default address, as a field element below
/// `2^128`. Mirrors `defaultDiversifier` in `sdk/src/keys/diversifier.ts`:
///
/// ```text
/// dvk     = blake2b-128("lelantos.addr.dvk.v1" || LE32(ivk))
/// d_bytes = AES-128-encrypt_dvk(LE4(0) || 0^12)
/// d       = d_bytes read little-endian
/// ```
pub fn default_diversifier(ivk: &Field) -> Field {
    let mut ivk_le = *ivk;
    ivk_le.reverse();
    let dvk = Blake2b::<U16>::new()
        .chain_update(DVK_DOMAIN)
        .chain_update(ivk_le)
        .finalize();

    // Index 0 in the leading four bytes, so the whole block is zero.
    let mut block = aes::Block::default();
    Aes128::new(&dvk).encrypt_block(&mut block);

    le_to_field(&block)
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

fn hash_to_field(inputs: &[Fq]) -> Result<Field, PoseidonError> {
    poseidon::hash(inputs).map(fq_to_be)
}
