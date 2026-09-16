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

use crate::clue::{pack, scalar_mul, unpack};
use crate::tree::Field;
use ark_ed_on_bn254::Fr;
use ark_ff::{Field as _, PrimeField};
use blake2::Blake2b;
use blake2::digest::Digest;
use blake2::digest::consts::{U12, U32};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use zeroize::Zeroizing;

const KDF_DOMAIN: &[u8] = b"lelantos.note.kdf.v1";
const NONCE_DOMAIN: &[u8] = b"lelantos.note.nonce.v1";

/// `8^-1` in the scalar field, for cofactor-cleared ECDH. See [`try_decrypt`].
fn inv8() -> Fr {
    static V: std::sync::OnceLock<Fr> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        Fr::from(8u64)
            .inverse()
            .expect("8 is invertible mod the subgroup order")
    })
}

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
    // Cofactor cleared, not checked. Baby-Jubjub is `Z_8 x Z_n`, so a sender may
    // pick `epk = T + [t]B8` with `T` in the 8-torsion. Under a plain `[ivk]epk`
    // that gives `shared = [ivk]T + [t]pk_d`, whose second term follows from the
    // published address and whose first has only eight values — eight crafted
    // notes, one of which decrypts, would reveal `ivk mod 8`.
    //
    // `[8]epk` annihilates the torsion term and `ivk * 8^-1` undoes the cofactor
    // on the prime-order part, so `shared` is `[ivk]epk` for an honest point and
    // carries no torsion term for a crafted one.
    //
    // The cofactor must be cleared on the *point*: `Fr` reduces modulo `n`, so
    // folding 8 into the scalar would be reduced away and silently restore the
    // leak with every test still passing.
    let epk = unpack(epk_packed).ok()?;
    let cleared = scalar_mul(epk, Fr::from(8u64));
    // Pure torsion: `shared` would be the identity for every `ivk` — one note
    // that decrypts in every wallet and is readable by any observer.
    if cleared.is_identity() {
        return None;
    }

    let shared = scalar_mul(cleared, Fr::from_be_bytes_mod_order(ivk) * inv8());
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

/// The sender half of the wire format, for `epk` values the SDK's encrypt path
/// cannot produce. Test-only, and here rather than in `tests.rs` so the KDF
/// domains and hash helpers stay private to this module.
#[cfg(test)]
pub(super) fn seal(
    epk_packed: &[u8; 32],
    shared: &crate::clue::CircomPoint,
    plaintext: &[u8],
) -> Vec<u8> {
    use chacha20poly1305::aead::Aead;
    let shared_packed = pack(shared);
    let key = blake2b_32(&[KDF_DOMAIN, epk_packed, &shared_packed]);
    let cipher = ChaCha20Poly1305::new(&Key::from(key));
    let nonce = Nonce::from(blake2b_12(&[NONCE_DOMAIN, epk_packed]));
    cipher.encrypt(&nonce, plaintext).expect("seal")
}
