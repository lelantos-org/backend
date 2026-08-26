use crate::clue::{
    fr_from_dec, point_from_xy, test_clue as test_clue_inner,
    test_clue_batch as test_clue_batch_inner,
};
use ark_ed_on_bn254::{Fq, Fr};
use ark_ff::{BigInteger, PrimeField};
use std::str::FromStr;

/// Test whether a clue matches a serialized detection key.
///
/// - `detection_key`: `gamma` Fr scalars concatenated, each 32 bytes
///   little-endian.
/// - `r_x`, `r_y`: Baby-Jubjub coordinates of the clue's randomness commitment
///   R, in the decimal-string form emitted by the contract event.
/// - `clue_bits`: 16-bit little-endian packed FMD bits.
/// - `gamma`: number of bits used.
pub fn test_clue(detection_key: &[u8], r_x: Fq, r_y: Fq, clue_bits: u16, gamma: usize) -> bool {
    let Some(dk) = parse_detection_key(detection_key, gamma) else {
        return false;
    };
    test_clue_parsed(&dk, r_x, r_y, clue_bits, gamma)
}

/// Pre-parse a serialized detection key into Fr scalars. Returns `None` when the
/// length does not match `gamma * 32`. Called once per subscription so the inner
/// filter loop can pass `&[Fr]` directly.
pub fn parse_detection_key(detection_key: &[u8], gamma: usize) -> Option<Vec<Fr>> {
    if detection_key.len() != gamma * 32 {
        return None;
    }
    let mut dk: Vec<Fr> = Vec::with_capacity(gamma);
    for chunk in detection_key.chunks_exact(32) {
        dk.push(Fr::from_le_bytes_mod_order(chunk));
    }
    Some(dk)
}

/// `test_clue` variant accepting a pre-parsed scalar list, avoiding the per-call
/// allocation and `from_le_bytes_mod_order` work in hot loops.
pub fn test_clue_parsed(dk: &[Fr], r_x: Fq, r_y: Fq, clue_bits: u16, gamma: usize) -> bool {
    let r = point_from_xy(r_x, r_y);
    test_clue_inner(dk, r, clue_bits, gamma)
}

/// Batched variant testing one clue against many pre-parsed detection keys.
/// Returns a `Vec<bool>` aligned with `dks`. Amortizes a fixed-base window table
/// over all keys and culls survivors bit by bit, so it suits scanning one clue
/// against a large subscriber set.
pub fn test_clue_batch_parsed(
    dks: &[&[Fr]],
    r_x: Fq,
    r_y: Fq,
    clue_bits: u16,
    gamma: usize,
) -> Vec<bool> {
    let r = point_from_xy(r_x, r_y);
    test_clue_batch_inner(dks, r, clue_bits, gamma)
}

/// Build detection key bytes from decimal-encoded scalars.
pub fn detection_key_from_dec(scalars: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(scalars.len() * 32);
    for s in scalars {
        let fr = fr_from_dec(s);
        let mut bytes = fr.into_bigint().to_bytes_le();
        bytes.resize(32, 0);
        out.extend_from_slice(&bytes);
    }
    out
}

/// Parse a decimal Fq string.
pub fn fq_from_decimal(s: &str) -> Fq {
    Fq::from_str(s).expect("fq decimal")
}
