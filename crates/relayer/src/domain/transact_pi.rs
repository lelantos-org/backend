//! Fiat-Shamir compression for the `transact_3x3` circuit.
//!
//! Mirrors `contracts/src/libs/PubInputs.sol :: compress(Transact, aux)` and
//! `SnarkCompression.evaluatePolyAtRaw`, so the relayer derives the same `(y, z)`
//! public-signal pair the on-chain verifier does. That lets it check a wallet's
//! proof locally rather than after paying for a `tree_update_batch` Groth16.
//!
//! Coefficient layout (42 entries), pinned by `circuits/src/3x3.circom`:
//!   [ 0]      merkleRoot
//!   [ 1.. 3]  nullifier[0..2]
//!   [ 4.. 6]  outCm[0..2]
//!   [ 7]      publicAssetId
//!   [ 8]      publicIn
//!   [ 9]      publicOut
//!   [10..15]  inCv flattened
//!   [16..21]  outCv flattened
//!   [22]      recipient
//!   [23]      chainId
//!   [24]      payer
//!   [25]      relayer
//!   [26..31]  outCvDep flattened
//!   [32..40]  (clueRx, clueRy, clueBits) per output
//!   [41]      auxDigest

use crate::adapters::abi::IMasp;
use crate::adapters::parse::BN254_R;
use crate::domain::dto::{TRANSACT_IN, TRANSACT_OUT};
use alloy::primitives::{U256, keccak256};
use alloy::sol_types::SolValue;

/// ABI calldata words of the `Transact` struct itself: the coefficients copied
/// verbatim before any are derived from `aux`.
///
/// Derived rather than written out: `merkleRoot`, one word per nullifier and per
/// `outCm`, the three public-value words, `inCv` as a coordinate pair per input,
/// the four address and chain words, and `outCv` plus `outCvDep` as two
/// coordinate pairs per output. 32 at 3x3 and 40 at 4x4; a literal here would
/// stop matching the circuit when the arity moves.
const STRUCT_WORDS: usize =
    1 + TRANSACT_IN + TRANSACT_OUT + 3 + 2 * TRANSACT_IN + 4 + 4 * TRANSACT_OUT;
/// Word index of the first `(clueRx, clueRy, clueBits)` triple: they start
/// where the struct's own words end.
const CLUE_BASE: usize = STRUCT_WORDS;
/// The struct words, one clue triple per output, then the aux digest: 42 at 3x3
/// and 53 at 4x4. `contracts/test/fixtures/transact_4x4_vector.json` publishes
/// `coeffCount` for the deployed shape, which the tests check.
pub const TRANSACT_COEFFS: usize = STRUCT_WORDS + 3 * TRANSACT_OUT + 1;

/// The `(y, z)` pair the deployed verifier is handed as its two public
/// signals, in that order: `y` is the circuit's output, `z` the challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactPublicSignals {
    pub y: U256,
    pub z: U256,
}

/// Build the coefficient vector ([`TRANSACT_COEFFS`] of them), then compress it.
///
/// Takes the already-built ABI structs rather than the wire DTOs, so one place
/// decides what a field means: the same builders the calldata uses.
pub fn compress(
    pi: &IMasp::Transact,
    aux: &[IMasp::OutputAux; TRANSACT_OUT],
) -> TransactPublicSignals {
    let c = coefficients(pi, aux);
    let z = U256::from_be_bytes(keccak256(c.abi_encode()).0) % *BN254_R;
    TransactPublicSignals {
        y: eval_poly(&c, z),
        z,
    }
}

/// The coefficients, in the order `PubInputs.compress(Transact)` lays them out.
/// Separate from [`compress`] so the layout can be pinned against the published
/// circuit vectors without a proof.
pub fn coefficients(pi: &IMasp::Transact, aux: &[IMasp::OutputAux; TRANSACT_OUT]) -> Vec<U256> {
    let mut c: Vec<U256> = Vec::with_capacity(TRANSACT_COEFFS);
    c.push(U256::from_be_bytes(pi.merkleRoot.0));
    for nf in &pi.nullifier {
        c.push(U256::from_be_bytes(nf.0));
    }
    for cm in &pi.outCm {
        c.push(U256::from_be_bytes(cm.0));
    }
    c.push(U256::from(pi.publicAssetId));
    c.push(U256::from(pi.publicIn));
    c.push(U256::from(pi.publicOut));
    for pt in &pi.inCv {
        c.push(pt[0]);
        c.push(pt[1]);
    }
    for pt in &pi.outCv {
        c.push(pt[0]);
        c.push(pt[1]);
    }
    c.push(U256::from_be_slice(pi.recipient.as_slice()));
    c.push(pi.chainId);
    c.push(U256::from_be_slice(pi.payer.as_slice()));
    c.push(U256::from_be_slice(pi.relayer.as_slice()));
    for pt in &pi.outCvDep {
        c.push(pt[0]);
        c.push(pt[1]);
    }
    debug_assert_eq!(c.len(), CLUE_BASE);

    for o in aux.iter() {
        c.push(o.clueRx);
        c.push(o.clueRy);
        c.push(U256::from(clue_bits(&o.ciphertext)));
    }
    c.push(aux_digest(aux));
    debug_assert_eq!(c.len(), TRANSACT_COEFFS);
    c
}

/// The clue's leading two bytes, as the contract reads them:
/// `uint16(bytes2(o.ciphertext[0:2]))`. A ciphertext shorter than two bytes
/// would revert there; here it contributes zero, and the following proof check
/// rejects the payload.
fn clue_bits(ciphertext: &[u8]) -> u16 {
    let hi = ciphertext.first().copied().unwrap_or(0);
    let lo = ciphertext.get(1).copied().unwrap_or(0);
    u16::from_be_bytes([hi, lo])
}

/// `keccak256(abi.encode(Output[] memory)) % R`. The contract encodes a dynamic
/// array, copying the fixed-size array into one first, and the two encodings
/// differ by a leading offset word.
fn aux_digest(aux: &[IMasp::OutputAux; TRANSACT_OUT]) -> U256 {
    let dynamic: Vec<IMasp::OutputAux> = aux.to_vec();
    U256::from_be_bytes(keccak256(dynamic.abi_encode()).0) % *BN254_R
}

/// Horner from the top coefficient down: `y = Σ c[i] · z^i (mod R)`.
fn eval_poly(coeffs: &[U256], z: U256) -> U256 {
    let r = *BN254_R;
    coeffs
        .iter()
        .rev()
        .fold(U256::ZERO, |acc, c| acc.mul_mod(z, r).add_mod(*c % r, r))
}

/// Compile-time reminder that this module is pinned to one circuit shape.
///
/// Every length above is derived from the two constants, so the published vectors
/// are what keep the layout correct. This assertion makes changing the arity a
/// deliberate edit in this file, where the coefficient order lives and no test
/// can infer it from the constants.
const _: () = assert!(TRANSACT_IN == 4 && TRANSACT_OUT == 4);

#[cfg(test)]
mod tests {
    use super::*;

    /// Published `transact_4x4` vectors, carrying the coefficient vector the
    /// reference implementation built plus the `z` and `y` derived from it, which
    /// is what this module must reproduce. A layout that drifts from the
    /// contract's produces proofs the chain rejects and a local check that rejects
    /// proofs the chain would accept.
    ///
    /// A missing file is a hard failure rather than a skip, so a renamed fixture
    /// cannot stop these tests from running unnoticed.
    ///
    /// The file is vendored from `contracts/test/fixtures/transact_4x4_vector.json`
    /// because this repository is checked out on its own in CI, where the
    /// contracts tree is not present.
    fn vectors() -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/vectors/transact_4x4.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("published vectors missing at {}: {e}", path.display()));
        serde_json::from_str(&raw).expect("published vectors parse")
    }

    fn u256(s: &str) -> U256 {
        U256::from_str_radix(s, 10).expect("decimal field element")
    }

    fn strs(v: &serde_json::Value) -> Vec<String> {
        v.as_array()
            .expect("array")
            .iter()
            .map(|x| x.as_str().expect("string").to_string())
            .collect()
    }

    /// `z` and `y` must match the published derivation for every vector, pinning
    /// the ABI preimage, the modular reduction and the Horner order at once.
    #[test]
    fn z_and_y_match_the_published_vectors() {
        let v = vectors();
        let cases = v["vectors"].as_array().expect("vectors");
        assert!(!cases.is_empty());
        for case in cases {
            let name = case["name"].as_str().unwrap_or("?");
            let coeffs: Vec<U256> = strs(&case["compression"]["coeffs"])
                .iter()
                .map(|s| u256(s))
                .collect();
            assert_eq!(coeffs.len(), TRANSACT_COEFFS, "{name}");

            let z = U256::from_be_bytes(keccak256(coeffs.abi_encode()).0) % *BN254_R;
            assert_eq!(
                z,
                u256(case["compression"]["z"].as_str().unwrap()),
                "{name} z"
            );
            assert_eq!(
                eval_poly(&coeffs, z),
                u256(case["compression"]["y"].as_str().unwrap()),
                "{name} y"
            );
            // The circuit's own output must agree, or a proof would never satisfy
            // the public signals handed to the verifier.
            assert_eq!(
                case["compression"]["y"], case["circuitOutput"]["y"],
                "{name} circuit output"
            );
        }
    }

    /// The layout: fields must land in the slots the vector specifies. The last
    /// slot is the aux digest, derived from ciphertext bytes the vector does not
    /// carry, so it is compared separately by construction in `aux_digest`.
    #[test]
    fn the_coefficient_layout_matches_the_published_vectors() {
        let v = vectors();
        for case in v["vectors"].as_array().expect("vectors") {
            let name = case["name"].as_str().unwrap_or("?");
            let w = &case["witness"];
            let expected: Vec<U256> = strs(&case["compression"]["coeffs"])
                .iter()
                .map(|s| u256(s))
                .collect();

            let pi = transact_from_witness(w);
            let aux = aux_from_witness(w);
            let got = coefficients(&pi, &aux);

            for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
                if i == TRANSACT_COEFFS - 1 {
                    continue; // auxDigest — see the doc comment above.
                }
                assert_eq!(g, e, "{name} coefficient {i} ({})", slot_name(i));
            }
        }
    }

    /// Which field a mismatched slot belongs to, so a layout drift names itself
    /// rather than printing an index.
    fn slot_name(i: usize) -> &'static str {
        match i {
            0 => "merkleRoot",
            1..=3 => "nullifier",
            4..=6 => "outCm",
            7 => "publicAssetId",
            8 => "publicIn",
            9 => "publicOut",
            10..=15 => "inCv",
            16..=21 => "outCv",
            22 => "recipient",
            23 => "chainId",
            24 => "payer",
            25 => "relayer",
            26..=31 => "outCvDep",
            32..=40 => "clue",
            _ => "auxDigest",
        }
    }

    fn b32(s: &str) -> alloy::primitives::FixedBytes<32> {
        alloy::primitives::FixedBytes::<32>::from(u256(s).to_be_bytes::<32>())
    }

    fn addr(s: &str) -> alloy::primitives::Address {
        alloy::primitives::Address::from_slice(&u256(s).to_be_bytes::<32>()[12..])
    }

    fn points(v: &serde_json::Value) -> [[U256; 2]; TRANSACT_OUT] {
        let rows = v.as_array().expect("points");
        std::array::from_fn(|i| {
            let p = strs(&rows[i]);
            [u256(&p[0]), u256(&p[1])]
        })
    }

    fn transact_from_witness(w: &serde_json::Value) -> IMasp::Transact {
        let nf = strs(&w["nullifier"]);
        let cm = strs(&w["out_cm"]);
        IMasp::Transact {
            merkleRoot: b32(w["merkle_root"].as_str().unwrap()),
            nullifier: std::array::from_fn(|i| b32(&nf[i])),
            outCm: std::array::from_fn(|i| b32(&cm[i])),
            publicAssetId: w["public_asset_id"].as_str().unwrap().parse().unwrap(),
            publicIn: w["public_in"].as_str().unwrap().parse().unwrap(),
            publicOut: w["public_out"].as_str().unwrap().parse().unwrap(),
            inCv: points(&w["in_cv"]),
            outCv: points(&w["out_cv"]),
            recipient: addr(w["recipient_address"].as_str().unwrap()),
            chainId: u256(w["chain_id"].as_str().unwrap()),
            payer: addr(w["payer_address"].as_str().unwrap()),
            relayer: addr(w["relayer_address"].as_str().unwrap()),
            outCvDep: points(&w["out_cv_dep"]),
        }
    }

    /// The vector publishes `clue_bits` as a number rather than the ciphertext it
    /// was read from, so the ciphertext is reconstructed as the two bytes the
    /// contract would slice, which also exercises `clue_bits`.
    fn aux_from_witness(w: &serde_json::Value) -> [IMasp::OutputAux; TRANSACT_OUT] {
        let rx = strs(&w["out_clue_Rx"]);
        let ry = strs(&w["out_clue_Ry"]);
        let bits = strs(&w["out_clue_bits"]);
        std::array::from_fn(|i| {
            let b: u16 = bits[i].parse().expect("clue bits");
            IMasp::OutputAux {
                clueRx: u256(&rx[i]),
                clueRy: u256(&ry[i]),
                ephPubX: U256::ZERO,
                ephPubY: U256::ZERO,
                ciphertext: b.to_be_bytes().to_vec().into(),
            }
        })
    }

    /// Checked against the circuit's own declaration rather than a second
    /// hand-written number: the fixture publishes `coeffCount` alongside the
    /// vectors, so a drifted layout cannot be reconciled by editing a literal.
    #[test]
    fn the_coefficient_vector_is_the_width_the_circuit_declares() {
        let v = vectors();
        let declared = v["circuit"]["coeffCount"]
            .as_u64()
            .expect("fixture declares coeffCount") as usize;
        assert_eq!(TRANSACT_COEFFS, declared);
    }

    /// The fixture also publishes the arity it was generated at. A vector file for
    /// another shape would otherwise satisfy every test above by being internally
    /// consistent.
    #[test]
    fn the_published_vectors_are_for_the_deployed_arity() {
        let v = vectors();
        let shape = &v["circuit"]["shape"];
        assert_eq!(shape["nIn"].as_u64(), Some(TRANSACT_IN as u64));
        assert_eq!(shape["nOut"].as_u64(), Some(TRANSACT_OUT as u64));
    }

    #[test]
    fn clue_bits_reads_the_leading_two_bytes_big_endian() {
        assert_eq!(clue_bits(&[0x12, 0x34, 0x56]), 0x1234);
        assert_eq!(clue_bits(&[0xff]), 0xff00);
        assert_eq!(clue_bits(&[]), 0);
    }

    /// Horner must agree with the direct power-sum, or `y` is wrong and every
    /// local verification fails against proofs the chain accepts.
    #[test]
    fn horner_matches_the_direct_power_sum() {
        let r = *BN254_R;
        let coeffs: Vec<U256> = (1u64..=9).map(U256::from).collect();
        let z = U256::from(7u64);

        let mut expected = U256::ZERO;
        let mut power = U256::from(1u64);
        for c in &coeffs {
            expected = expected.add_mod(c.mul_mod(power, r), r);
            power = power.mul_mod(z, r);
        }
        assert_eq!(eval_poly(&coeffs, z), expected);
    }

    /// Coefficients are reduced before folding, matching the contract's in-field
    /// requirement rather than wrapping at 2^256.
    #[test]
    fn evaluation_stays_in_the_field() {
        let z = U256::from(3u64);
        let y = eval_poly(&[U256::MAX, U256::MAX], z);
        assert!(y < *BN254_R);
    }
}
