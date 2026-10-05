//! SDK/backend agreement on diversified addresses and seed-derived outputs.
//!
//! [`tests/vectors/diversified.json`](../../../tests/vectors/diversified.json)
//! is a copy of `sdk/tests/vectors/diversified.json`, emitted by the SDK's
//! `scripts/gen-diversified-vectors.ts`. Re-copy it when the SDK regenerates.

use ark_ed_on_bn254::Fr;
use crypto::clue::{GAMMA, detection_key, fr_from_dec, pack};
use crypto::note::{
    NotePlaintext, address_keys, diversified_base, expand_seed, split_clue_prefix, try_decrypt,
};
use crypto::tree::Field;
use num_bigint::BigUint;
use serde::Deserialize;
use std::str::FromStr;

#[derive(Deserialize)]
struct VectorFile {
    addresses: Vec<Address>,
    diversified_base: Vec<Base>,
    fmd: Fmd,
    seed: Vec<Seed>,
}

#[derive(Deserialize)]
struct Address {
    label: String,
    ivk_dec: String,
    d_dec: String,
    g_d_packed_hex: String,
    pk_dec: String,
    pk_d_packed_hex: String,
    ck_d_packed_hex: String,
}

#[derive(Deserialize)]
struct Base {
    d_dec: String,
    g_d_packed_hex: String,
}

#[derive(Deserialize)]
struct Fmd {
    detection: Vec<Detection>,
}

#[derive(Deserialize)]
struct Detection {
    ivk_dec: String,
    dk_root_dec: String,
    gamma: usize,
    x_dec: Vec<String>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct Seed {
    address_index: usize,
    rho_dec: String,
    asset_dec: String,
    value_dec: String,
    rseed_hex: String,
    rcm_dec: String,
    esk_dec: String,
    fmd_r_dec: String,
    epk_packed_hex: String,
    clue_R_packed_hex: String,
    clue_bits_hex: String,
    plaintext_hex: String,
    ciphertext_hex: String,
}

fn load() -> VectorFile {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/diversified.json"
    ))
    .expect("vectors");
    serde_json::from_str(&text).expect("parse")
}

/// A decimal integer in this crate's big-endian field form.
fn field(dec: &str) -> Field {
    let be = BigUint::from_str(dec).expect("decimal").to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - be.len()..].copy_from_slice(&be);
    out
}

fn h2b(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).expect("hex")
}

fn bytes32(s: &str) -> [u8; 32] {
    h2b(s).try_into().expect("32 bytes")
}

#[test]
fn diversified_base_matches_the_sdk() {
    for v in load().diversified_base {
        let g_d = diversified_base(&field(&v.d_dec)).expect("poseidon");
        assert_eq!(pack(&g_d), bytes32(&v.g_d_packed_hex), "d = {}", v.d_dec);
    }
}

#[test]
fn address_keys_match_the_sdk() {
    for v in load().addresses {
        let keys = address_keys(&field(&v.ivk_dec), &field(&v.d_dec)).expect("poseidon");
        assert_eq!(pack(&keys.g_d), bytes32(&v.g_d_packed_hex), "{}", v.label);
        assert_eq!(keys.pk, field(&v.pk_dec), "{}", v.label);
        assert_eq!(pack(&keys.pk_d), bytes32(&v.pk_d_packed_hex), "{}", v.label);
        assert_eq!(pack(&keys.ck_d), bytes32(&v.ck_d_packed_hex), "{}", v.label);
    }
}

#[test]
fn detection_key_matches_the_sdk() {
    let mut checked = 0;
    for v in load().fmd.detection.iter().filter(|v| v.gamma == GAMMA) {
        let expected: Vec<Fr> = v.x_dec.iter().map(|x| fr_from_dec(x)).collect();
        assert_eq!(
            detection_key(fr_from_dec(&v.dk_root_dec)),
            expected,
            "ivk = {}",
            v.ivk_dec
        );
        // The key does not depend on the diversifier, so any one does.
        let keys = address_keys(&field(&v.ivk_dec), &field("0")).expect("poseidon");
        assert_eq!(keys.detection_key, expected, "ivk = {}", v.ivk_dec);
        checked += 1;
    }
    assert!(checked > 0, "no detection vector at the sender gamma");
}

/// The recipient's view of a sealed output: its ciphertext opens under `ivk`,
/// and the seed in the plaintext yields the blinder, ephemeral key and clue the
/// sender published.
#[test]
fn a_sealed_output_opens_to_what_its_seed_yields() {
    let f = load();
    for (i, v) in f.seed.iter().enumerate() {
        let addr = &f.addresses[v.address_index];
        let ivk = field(&addr.ivk_dec);

        let wire = h2b(&v.ciphertext_hex);
        let (clue_bits, body) = split_clue_prefix(&wire).expect("clue prefix");
        let plaintext = try_decrypt(&ivk, &bytes32(&v.epk_packed_hex), body)
            .unwrap_or_else(|| panic!("seed {i}: the recipient's ivk failed to decrypt"));
        assert_eq!(plaintext, h2b(&v.plaintext_hex), "seed {i}");

        let note = NotePlaintext::decode(&plaintext).expect("224-byte plaintext");
        assert_eq!(note.asset_id.to_string(), v.asset_dec, "seed {i}");
        assert_eq!(note.value.to_string(), v.value_dec, "seed {i}");
        assert_eq!(note.rho, field(&v.rho_dec), "seed {i}");
        assert_eq!(note.rseed, bytes32(&v.rseed_hex), "seed {i}");
        assert_eq!(note.d, field(&addr.d_dec), "seed {i}");

        let seed = expand_seed(&note.rseed, &note.rho);
        assert_eq!(seed.rcm, field(&v.rcm_dec), "seed {i}");
        assert_eq!(seed.esk, fr_from_dec(&v.esk_dec), "seed {i}");
        assert_eq!(seed.fmd_r, fr_from_dec(&v.fmd_r_dec), "seed {i}");

        let keys = address_keys(&ivk, &note.d).expect("poseidon");
        let published = seed.published(&keys);
        assert_eq!(pack(&published.epk), bytes32(&v.epk_packed_hex), "seed {i}");
        assert_eq!(
            pack(&published.clue_r),
            bytes32(&v.clue_R_packed_hex),
            "seed {i}"
        );
        assert_eq!(published.clue_bits, clue_bits, "seed {i}");
        assert_eq!(
            published.clue_bits,
            u16::from(h2b(&v.clue_bits_hex)[0]),
            "seed {i}"
        );
    }
}
