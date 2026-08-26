//! Trial decryption of an encrypted output note.
//!
//! Wire format, which must match `sdk/src/notes/encrypt.ts` and
//! `sdk/wasm/jubjub/src/decrypt.rs` byte for byte:
//!
//! ```text
//! key   = blake2b("lelantos.note.kdf.v1"   || epk_packed || shared_packed, 32B)
//! nonce = blake2b("lelantos.note.nonce.v1" || epk_packed, 12B)
//! ct    = ChaCha20-Poly1305(key, nonce, plaintext)
//! ```
//!
//! The sender picks `esk` and sets `epk = esk·B8`, `shared = esk·pk_d`. The
//! holder of `ivk` recovers the same `shared` as `ivk·epk`, since
//! `pk_d = ivk·B8`. `epk` is fresh per note, so the key is single-use and the
//! nonce cannot repeat; deriving the nonce from `epk` is defence in depth against
//! a path that reuses a key.

use crate::clue::{pack, scalar_mul, unpack_subgroup};
use crate::tree::Field;
use ark_ed_on_bn254::Fr;
use ark_ff::PrimeField;
use blake2::Blake2b;
use blake2::digest::Digest;
use blake2::digest::consts::{U12, U32};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use zeroize::Zeroizing;

const KDF_DOMAIN: &[u8] = b"lelantos.note.kdf.v1";
const NONCE_DOMAIN: &[u8] = b"lelantos.note.nonce.v1";

/// Recover the plaintext of a note encrypted to `ivk`'s address, or `None`.
///
/// `None` means only "not for this key". It covers an `epk` that will not
/// decompress, one outside the prime-order subgroup, and an AEAD tag that does
/// not verify. These are indistinguishable by design: a caller that reacted
/// differently to a malformed `epk` than to a foreign note would answer "is this
/// yours?" for any asker.
///
/// `ivk` is big-endian like every field element crossing this crate, and is
/// reduced modulo the subgroup order before use, matching `scalar_from_le` on the
/// wallet side. `epk_packed` is wire bytes: a compressed point rather than a
/// field element, so it stays in the little-endian form the wallet sent. `body`
/// is the ciphertext without the two-byte clue prefix; see
/// [`super::strip_clue_prefix`].
pub fn try_decrypt(ivk: &Field, epk_packed: &[u8; 32], body: &[u8]) -> Option<Vec<u8>> {
    // Subgroup check first, before anything secret-dependent runs. Baby-Jubjub is
    // `Z_8 x Z_n`, so a sender may pick `epk = T + [t]B8` with `T` in the
    // 8-torsion. Then `shared = [ivk]T + [t]pk_d`, whose second term follows from
    // the published address and whose first has only eight possible values, so
    // eight crafted notes would reveal `ivk mod 8`. Deferring the check until
    // after the AEAD would let a crafted note verify and make the extra work
    // observable as timing.
    let epk = unpack_subgroup(epk_packed).ok()?;

    let shared = scalar_mul(epk, Fr::from_be_bytes_mod_order(ivk));
    let shared_packed = Zeroizing::new(pack(&shared));
    let key = Zeroizing::new(blake2b_32(&[KDF_DOMAIN, epk_packed, &*shared_packed]));

    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    let nonce = Nonce::from(blake2b_12(&[NONCE_DOMAIN, epk_packed]));
    cipher.decrypt(&nonce, body).ok()
}

fn blake2b_32(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Blake2b::<U32>::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn blake2b_12(parts: &[&[u8]]) -> [u8; 12] {
    let mut h = Blake2b::<U12>::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}
