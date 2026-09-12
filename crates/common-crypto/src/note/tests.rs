//! Parity against the wallet.
//!
//! `tests/vectors/note-parity.json` is emitted by the SDK's encrypt path
//! (`sdk/src/notes/encrypt.ts` and `codec.ts`), so these assertions pin this
//! module to the format a real wallet produces rather than to a second reading of
//! the spec. Regenerate it from the SDK if the note format changes; doing so
//! invalidates every issued proof.

use super::*;
use crate::clue::{
    CircomPoint, base8_circom, pack, point_from_xy, scalar_mul, unpack, unpack_subgroup,
};
use crate::note::decrypt::seal;
use crate::tree::fq_to_be;
use ark_ed_on_bn254::{Fq, Fr};
use ark_ff::Field as _;
use serde::Deserialize;
use std::str::FromStr;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vector {
    ivk_le_hex: String,
    pk_dec: String,
    epk_le_hex: String,
    wire_ciphertext_hex: String,
    plaintext_hex: String,
    asset_id: String,
    value: String,
    nf0_dec: String,
    out_index: u64,
    rho_dec: String,
    rcm_dec: String,
    rcv_dep_dec: String,
    cm_dec: String,
}

fn vectors() -> Vec<Vector> {
    let raw = include_str!("../../tests/vectors/note-parity.json");
    serde_json::from_str(raw).expect("note-parity.json parses")
}

fn bytes32(hex_str: &str) -> [u8; 32] {
    let v = hex::decode(hex_str).expect("hex");
    v.try_into().expect("32 bytes")
}

/// A decimal field element in this crate's big-endian wire form.
fn fq(dec: &str) -> crate::tree::Field {
    fq_to_be(Fq::from_str(dec).expect("decimal field element"))
}

/// The SDK spells `ivk` little-endian; this crate takes it big-endian, like every
/// other field element crossing its boundary.
fn ivk_be(hex_le: &str) -> crate::tree::Field {
    let mut b = bytes32(hex_le);
    b.reverse();
    b
}

/// The recipient-side pipeline end to end against wallet-produced bytes: strip
/// the clue prefix, trial-decrypt, decode, and rebuild the commitment the proof
/// would have carried.
#[test]
fn decrypts_and_rebuilds_the_commitment_from_sdk_vectors() {
    for (i, v) in vectors().iter().enumerate() {
        let ivk = ivk_be(&v.ivk_le_hex);
        let epk = bytes32(&v.epk_le_hex);
        let wire = hex::decode(&v.wire_ciphertext_hex).expect("hex");

        let body = strip_clue_prefix(&wire).expect("wire carries a clue prefix");
        let plaintext = try_decrypt(&ivk, &epk, body).unwrap_or_else(|| {
            panic!("vector {i}: the recipient's own ivk failed to decrypt the note")
        });
        assert_eq!(hex::encode(&plaintext), v.plaintext_hex, "vector {i}");

        let note = NotePlaintext::decode(&plaintext).expect("112-byte plaintext");
        assert_eq!(note.asset_id.to_string(), v.asset_id, "vector {i}");
        assert_eq!(note.value.to_string(), v.value, "vector {i}");
        assert_eq!(note.rho, fq(&v.rho_dec), "vector {i}");
        assert_eq!(note.rcm, fq(&v.rcm_dec), "vector {i}");
        assert_eq!(note.rcv_dep, fq(&v.rcv_dep_dec), "vector {i}");

        // `ivk` alone recovers the owner key, letting a party without a spending
        // key verify a payment to its own address.
        let pk = derive_pk(&ivk).expect("poseidon");
        assert_eq!(pk, fq(&v.pk_dec), "vector {i}");

        // The circuit pins output rho, so it is recomputable from public inputs
        // rather than taken from the plaintext.
        let rho = derive_rho(&fq(&v.nf0_dec), v.out_index).expect("poseidon");
        assert_eq!(rho, note.rho, "vector {i}");

        let cm =
            commitment(note.asset_id, note.value, &pk, &note.rho, &note.rcm).expect("poseidon");
        assert_eq!(cm, fq(&v.cm_dec), "vector {i}");
    }
}

/// A note addressed to another key must read as not ours, never as an error or a
/// partial decode.
#[test]
fn a_foreign_ivk_yields_nothing() {
    let vs = vectors();
    let mine = &vs[0];
    let theirs = &vs[1];
    let wire = hex::decode(&mine.wire_ciphertext_hex).expect("hex");
    let body = strip_clue_prefix(&wire).expect("prefix");

    assert!(
        try_decrypt(
            &ivk_be(&theirs.ivk_le_hex),
            &bytes32(&mine.epk_le_hex),
            body
        )
        .is_none()
    );
}

/// Flipping one byte of the AEAD body must fail the tag rather than yield a
/// mangled note, which is what makes a decrypted plaintext trustworthy.
#[test]
fn a_tampered_ciphertext_fails_the_tag() {
    let v = &vectors()[0];
    let mut wire = hex::decode(&v.wire_ciphertext_hex).expect("hex");
    let last = wire.len() - 1;
    wire[last] ^= 0x01;

    let body = strip_clue_prefix(&wire).expect("prefix");
    assert!(try_decrypt(&ivk_be(&v.ivk_le_hex), &bytes32(&v.epk_le_hex), body).is_none());
}

/// A crafted `epk = T + [t]B8` yields the same shared secret as `[t]B8` alone.
///
/// The torsion term is annihilated arithmetically by `[8]epk`, so the crafted
/// point decrypts a note keyed on the torsion-free secret. A plain `[ivk]epk`
/// would compute `[ivk]T + [ivk][t]B8` and fail — which is the leak of
/// `ivk mod 8` this replaces.
#[test]
fn a_crafted_epk_has_its_torsion_term_annihilated() {
    let v = &vectors()[0];
    let ivk_be = ivk_be(&v.ivk_le_hex);
    let ivk = Fr::from_be_bytes_mod_order(&ivk_be);

    let t = Fr::from(12345u64);
    let q = scalar_mul(base8_circom(), t);
    let mixed = add_circom(q, order_two());
    assert!(mixed.is_on_curve());
    assert!(
        !mixed.is_in_prime_subgroup(),
        "the crafted point must be off-subgroup"
    );

    // Keyed on the torsion-free secret, with the crafted `epk` bound into the
    // KDF exactly as the wire format requires.
    let shared = scalar_mul(q, ivk);
    let epk_packed = pack(&mixed);
    let body = seal(
        &epk_packed,
        &shared,
        b"a 112-byte plaintext is not needed here",
    );

    assert_eq!(
        try_decrypt(&ivk_be, &epk_packed, &body).as_deref(),
        Some(&b"a 112-byte plaintext is not needed here"[..]),
    );
}

/// The 8-torsion subgroup, circomlibjs-packed. Orders 1, 2, 4, 4, 8, 8, 8, 8.
///
/// Obtained as `[n]R` for points `R` off the prime-order subgroup; each is
/// re-derived as 8-torsion below rather than trusted. The same set is used by
/// the SDK's `notes/cofactor.test.ts`.
const TORSION: [&str; 8] = [
    "0100000000000000000000000000000000000000000000000000000000000000",
    "000000f093f5e1439170b97948e833285d588181b64550b829a031e1724e6430",
    "0000000000000000000000000000000000000000000000000000000000000000",
    "77d6d0af811efdaba0b534826dc591b72c94a64b7d12c16314d3721121b7ab0a",
    "8a292f4012d7e497f0ba84f7da22a27030c4da3539338f5415cdbecf5197b825",
    "77d6d0af811efdaba0b534826dc591b72c94a64b7d12c16314d3721121b7ab8a",
    "8a292f4012d7e497f0ba84f7da22a27030c4da3539338f5415cdbecf5197b8a5",
    "0000000000000000000000000000000000000000000000000000000000000080",
];

/// The fixtures really are the 8-torsion subgroup: eight distinct points, each
/// killed by `[8]`.
#[test]
fn the_torsion_fixtures_are_eight_torsion() {
    let mut seen = std::collections::HashSet::new();
    for hex_str in TORSION {
        let packed = bytes32(hex_str);
        let t = unpack(&packed).expect("torsion point decodes");
        assert!(
            scalar_mul(t, Fr::from(8u64)).is_identity(),
            "fixture is not 8-torsion"
        );
        assert!(seen.insert(packed), "duplicate fixture");
    }
}

/// Every pure-torsion `epk` is refused, including the identity.
///
/// Its shared secret is the identity for *every* `ivk`, so one such note would
/// decrypt in every wallet and be readable by any observer. This is the note
/// path's own rejection, not `unpack_subgroup`'s — that is no longer on it.
#[test]
fn a_pure_torsion_epk_is_refused() {
    let v = &vectors()[0];
    let ivk_be = ivk_be(&v.ivk_le_hex);
    let identity = point_from_xy(Fq::ZERO, Fq::ONE);

    for hex_str in TORSION {
        let epk_packed = bytes32(hex_str);
        // Sealed under the identity, which is what the ECDH would produce.
        let body = seal(&epk_packed, &identity, b"unreachable");
        assert!(
            try_decrypt(&ivk_be, &epk_packed, &body).is_none(),
            "pure-torsion epk must be refused: {hex_str}"
        );
    }
}

/// The identity absorbs every scalar, so a shared secret derived from it is the
/// same for all keys.
///
/// `unpack` accepts it — it satisfies the curve equation — so the note path
/// rejects it on the cleared point instead. `unpack_subgroup` still refuses it
/// for the callers that decode addresses.
#[test]
fn the_identity_is_rejected_as_an_ephemeral_key() {
    let packed = pack(&point_from_xy(Fq::ZERO, Fq::ONE));
    assert!(unpack(&packed).is_ok());
    assert!(unpack_subgroup(&packed).is_err());

    let v = &vectors()[0];
    let body = seal(&packed, &point_from_xy(Fq::ZERO, Fq::ONE), b"unreachable");
    assert!(try_decrypt(&ivk_be(&v.ivk_le_hex), &packed, &body).is_none());
}

#[test]
fn a_plaintext_of_the_wrong_length_is_refused() {
    assert!(NotePlaintext::decode(&[0u8; NOTE_PLAINTEXT_BYTES - 1]).is_none());
    assert!(NotePlaintext::decode(&[0u8; NOTE_PLAINTEXT_BYTES + 1]).is_none());
    assert!(NotePlaintext::decode(&[0u8; NOTE_PLAINTEXT_BYTES]).is_some());
}

#[test]
fn a_ciphertext_shorter_than_the_clue_prefix_has_no_body() {
    assert!(strip_clue_prefix(&[]).is_none());
    assert!(strip_clue_prefix(&[0x00]).is_none());
    assert_eq!(strip_clue_prefix(&[0x00, 0x2a]), Some(&[][..]));
}

/// The order-2 point `(0, -1)`. On the curve, outside the prime-order subgroup.
fn order_two() -> CircomPoint {
    point_from_xy(Fq::ZERO, -Fq::ONE)
}

/// Twisted-Edwards addition in circomlib coordinates, for building a point the
/// public API offers no way to construct.
fn add_circom(p: CircomPoint, q: CircomPoint) -> CircomPoint {
    let a = Fq::from(crate::clue::COEFF_A_CIRCOM);
    let d = Fq::from(crate::clue::COEFF_D_CIRCOM);
    let (x1x2, y1y2) = (p.x * q.x, p.y * q.y);
    let dprod = d * x1x2 * y1y2;
    point_from_xy(
        (p.x * q.y + p.y * q.x) / (Fq::ONE + dprod),
        (y1y2 - a * x1x2) / (Fq::ONE - dprod),
    )
}
