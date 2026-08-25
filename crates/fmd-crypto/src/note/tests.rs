//! Parity against the wallet.
//!
//! `tests/vectors/note-parity.json` is emitted by the SDK's own encrypt path
//! (`sdk/src/notes/encrypt.ts` + `codec.ts`), so these assertions pin this
//! module to the format a real wallet produces rather than to a second reading
//! of the spec. Regenerate it from the SDK if the note format ever changes —
//! and expect every issued proof to be invalidated when it does.

use super::*;
use crate::clue::{
    CircomPoint, base8_circom, pack, point_from_xy, scalar_mul, unpack, unpack_subgroup,
};
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

/// The SDK spells `ivk` little-endian; this crate takes it big-endian, like
/// every other field element crossing its boundary.
fn ivk_be(hex_le: &str) -> crate::tree::Field {
    let mut b = bytes32(hex_le);
    b.reverse();
    b
}

/// The whole recipient-side pipeline, end to end, against wallet-produced
/// bytes: strip the clue prefix, trial-decrypt, decode, and rebuild the
/// commitment the proof would have carried.
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

        // `ivk` alone recovers the owner key, which is what lets a party that
        // holds no spending key verify a payment to its own address.
        let pk = derive_pk(&ivk).expect("poseidon");
        assert_eq!(pk, fq(&v.pk_dec), "vector {i}");

        // The circuit pins output rho, so it is recomputable from public
        // inputs and does not have to be taken from the plaintext.
        let rho = derive_rho(&fq(&v.nf0_dec), v.out_index).expect("poseidon");
        assert_eq!(rho, note.rho, "vector {i}");

        let cm =
            commitment(note.asset_id, note.value, &pk, &note.rho, &note.rcm).expect("poseidon");
        assert_eq!(cm, fq(&v.cm_dec), "vector {i}");
    }
}

/// Someone else's note must be silently not-ours, never an error and never a
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
/// mangled note. This is what makes the value in a decrypted plaintext worth
/// reading at all.
#[test]
fn a_tampered_ciphertext_fails_the_tag() {
    let v = &vectors()[0];
    let mut wire = hex::decode(&v.wire_ciphertext_hex).expect("hex");
    let last = wire.len() - 1;
    wire[last] ^= 0x01;

    let body = strip_clue_prefix(&wire).expect("prefix");
    assert!(try_decrypt(&ivk_be(&v.ivk_le_hex), &bytes32(&v.epk_le_hex), body).is_none());
}

/// An `epk` outside the prime-order subgroup is rejected before the ECDH runs.
///
/// Without this, eight submissions carrying `epk = T + [t]B8` for the eight
/// 8-torsion points `T` leak `ivk mod 8`: exactly one of them decrypts.
#[test]
fn an_eight_torsion_epk_is_rejected() {
    let mixed = add_circom(scalar_mul(base8_circom(), Fr::from(12345u64)), order_two());
    assert!(
        mixed.is_on_curve(),
        "the crafted point must still satisfy the curve equation"
    );
    assert!(!mixed.is_in_prime_subgroup());

    let packed = pack(&mixed);
    // It decompresses fine — which is the point: `unpack` alone would let it
    // through, and only the subgroup test stops it.
    assert!(unpack(&packed).is_ok());
    assert!(unpack_subgroup(&packed).is_err());

    let v = &vectors()[0];
    let wire = hex::decode(&v.wire_ciphertext_hex).expect("hex");
    let body = strip_clue_prefix(&wire).expect("prefix");
    assert!(try_decrypt(&ivk_be(&v.ivk_le_hex), &packed, body).is_none());
}

/// The identity absorbs every scalar, so a shared secret derived from it is the
/// same for all keys. `unpack` accepts it; `unpack_subgroup` must not.
#[test]
fn the_identity_is_rejected_as_an_ephemeral_key() {
    let packed = pack(&point_from_xy(Fq::ZERO, Fq::ONE));
    assert!(unpack(&packed).is_ok());
    assert!(unpack_subgroup(&packed).is_err());
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
/// public API deliberately offers no way to build.
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
