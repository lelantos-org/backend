//! Fiat-Shamir compression for the `transact_4x6` circuit.
//!
//! Mirrors `contracts/src/libs/PubInputs.sol :: compress(Transact, aux)` and
//! `SnarkCompression.evaluatePolyAtRaw`, so the relayer derives the same
//! `(y, digest, z)` public signals the on-chain verifier does. That lets it check
//! a wallet's proof locally rather than after paying for a `tree_update_batch`
//! Groth16.
//!
//! Two spans over one preimage. All 38 words are hashed into `z`; only the
//! leading 13, the coefficients, are evaluated into `y`. The word after them is
//! the circuit's commitment to those coefficients, which the payload supplies and
//! which is handed to the verifier as given. The rest bind to the proof through
//! `z` alone:
//!
//! ```text
//! [ 0]      merkleRoot                              coefficient
//! [ 1.. 4]  nullifier[0..3]                         coefficient
//! [ 5..10]  outCm[0..5]                             coefficient
//! [11]      publicAssetId                           coefficient
//! [12]      publicOut                               coefficient
//! [13]      digest                                  challenge only; second signal
//! [14]      recipient                               challenge only
//! [15]      chainId                                 challenge only
//! [16]      payer                                   challenge only
//! [17]      relayer                                 challenge only
//! [18]      intentHash                              challenge only
//! [19..36]  (clueRx, clueRy, clueBits) per output   challenge only
//! [37]      auxDigest                               challenge only
//! ```

use crate::adapters::abi::IMasp;
use crate::domain::dto::{TRANSACT_IN, TRANSACT_OUT};
use crate::domain::field::BN254_R;
use alloy::primitives::{U256, keccak256};
use alloy::sol_types::SolValue;

/// The words the circuit evaluates into `y`: `merkleRoot`, one word per
/// nullifier and per `outCm`, and the two public-value words. 13 at 4x6.
/// `contracts/test/fixtures/transact_4x6_vector.json` publishes `coeffCount` for
/// the deployed shape, which the tests check.
pub const TRANSACT_COEFFS: usize = 1 + TRANSACT_IN + TRANSACT_OUT + 2;
/// ABI calldata words of the `Transact` struct itself: the coefficients, the
/// digest, then the four address and chain words and `intentHash`. The clue
/// triples start here.
const STRUCT_WORDS: usize = TRANSACT_COEFFS + 6;
/// Every word hashed into `z`: the struct words, one clue triple per output,
/// then the aux digest. 38 at 4x6, published as `challengeWords`.
pub const TRANSACT_CHALLENGE_WORDS: usize = STRUCT_WORDS + 3 * TRANSACT_OUT + 1;

/// The deployed verifier's three public signals, in order: `y` and `digest` are
/// the circuit's outputs, `z` the challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactPublicSignals {
    pub y: U256,
    pub digest: U256,
    pub z: U256,
}

impl TransactPublicSignals {
    /// How many public signals the circuit publishes.
    pub const COUNT: usize = 3;

    /// The signals as big-endian words, in the order the verifier takes them.
    pub fn words(&self) -> [[u8; 32]; Self::COUNT] {
        [self.y, self.digest, self.z].map(|w| w.to_be_bytes())
    }
}

/// Build the challenge preimage ([`TRANSACT_CHALLENGE_WORDS`] of them), hash all
/// of it into `z`, and evaluate its leading [`TRANSACT_COEFFS`] into `y`. The
/// digest is `pi.digest`, never recomputed: a wrong one fails the proof.
///
/// Takes the already-built ABI structs rather than the wire DTOs, so one place
/// decides what a field means: the same builders the calldata uses.
pub fn compress(
    pi: &IMasp::Transact,
    aux: &[IMasp::OutputAux; TRANSACT_OUT],
) -> TransactPublicSignals {
    let c = challenge(pi, aux);
    let z = U256::from_be_bytes(keccak256(c.abi_encode()).0) % *BN254_R;
    TransactPublicSignals {
        y: eval_poly(&c[..TRANSACT_COEFFS], z),
        digest: pi.digest,
        z,
    }
}

/// The challenge preimage, in the order `PubInputs.compress(Transact)` lays it
/// out. Separate from [`compress`] so the layout can be pinned against the
/// published circuit vectors without a proof.
pub fn challenge(pi: &IMasp::Transact, aux: &[IMasp::OutputAux; TRANSACT_OUT]) -> Vec<U256> {
    let mut c: Vec<U256> = Vec::with_capacity(TRANSACT_CHALLENGE_WORDS);
    c.push(U256::from_be_bytes(pi.merkleRoot.0));
    for nf in &pi.nullifier {
        c.push(U256::from_be_bytes(nf.0));
    }
    for cm in &pi.outCm {
        c.push(U256::from_be_bytes(cm.0));
    }
    c.push(U256::from(pi.publicAssetId));
    c.push(U256::from(pi.publicOut));
    debug_assert_eq!(c.len(), TRANSACT_COEFFS);

    c.push(pi.digest);
    c.push(U256::from_be_slice(pi.recipient.as_slice()));
    c.push(pi.chainId);
    c.push(U256::from_be_slice(pi.payer.as_slice()));
    c.push(U256::from_be_slice(pi.relayer.as_slice()));
    // A full word, not an address: no 160-bit mask.
    c.push(pi.intentHash);
    debug_assert_eq!(c.len(), STRUCT_WORDS);

    for o in aux.iter() {
        c.push(o.clueRx);
        c.push(o.clueRy);
        c.push(U256::from(clue_bits(&o.ciphertext)));
    }
    c.push(aux_digest(aux));
    debug_assert_eq!(c.len(), TRANSACT_CHALLENGE_WORDS);
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

/// `keccak256(abi.encode(Output[] memory)) % R`: the aux array encoded as a
/// dynamic `tuple[]`, so its length is part of the preimage.
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
const _: () = assert!(TRANSACT_IN == 4 && TRANSACT_OUT == 6);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::calldata::build_aux;
    use crate::domain::dto::OutputAuxDto;
    use crypto::poseidon::coeff_digest;

    /// Published `transact_4x6` vectors, carrying the challenge preimage and
    /// coefficient vector the reference implementation built plus the `digest`,
    /// `z` and `y` derived from them, which is what this module must reproduce. A layout that
    /// drifts from the contract's produces proofs the chain rejects and a local
    /// check that rejects proofs the chain would accept.
    ///
    /// A missing file is a hard failure rather than a skip, so a renamed fixture
    /// cannot stop these tests from running unnoticed.
    ///
    /// The file is vendored from `circuits/vectors/transact-4x6.json` (the same
    /// bytes as `contracts/test/fixtures/transact_4x6_vector.json`) because this repository is checked out on its own in CI, where the
    /// contracts tree is not present.
    fn vectors() -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/vectors/transact_4x6.json");
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

    /// A published array of decimal field elements.
    fn words(v: &serde_json::Value) -> Vec<U256> {
        strs(v).iter().map(|s| u256(s)).collect()
    }

    /// `z`, `y` and `digest` must match the published derivation for every
    /// vector, pinning the ABI preimage, the modular reduction, the Horner order,
    /// the split between the hashed and the evaluated spans and the digest's
    /// place in the preimage at once.
    #[test]
    fn z_y_and_digest_match_the_published_vectors() {
        let v = vectors();
        let cases = v["vectors"].as_array().expect("vectors");
        assert!(!cases.is_empty());
        for case in cases {
            let name = case["name"].as_str().unwrap_or("?");
            let challenge = words(&case["compression"]["challenge"]);
            let coeffs = words(&case["compression"]["coeffs"]);
            assert_eq!(challenge.len(), TRANSACT_CHALLENGE_WORDS, "{name}");
            assert_eq!(coeffs.len(), TRANSACT_COEFFS, "{name}");
            assert_eq!(coeffs, challenge[..TRANSACT_COEFFS], "{name} coeffs prefix");

            // The digest word follows the coefficients and commits to them. The
            // fold is the one the batch path computes its own digest with.
            let digest = u256(case["compression"]["digest"].as_str().unwrap());
            assert_eq!(challenge[TRANSACT_COEFFS], digest, "{name} digest word");
            let bytes: Vec<[u8; 32]> = coeffs.iter().map(U256::to_be_bytes).collect();
            assert_eq!(
                U256::from_be_bytes(coeff_digest(&bytes).unwrap()),
                digest,
                "{name} digest"
            );

            let z = U256::from_be_bytes(keccak256(challenge.abi_encode()).0) % *BN254_R;
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
            // The circuit's own outputs must agree, or a proof would never satisfy
            // the public signals handed to the verifier.
            for signal in ["y", "digest"] {
                assert_eq!(
                    case["compression"][signal], case["circuitOutput"][signal],
                    "{name} circuit output {signal}"
                );
            }
        }
    }

    /// The layout: fields must land in the slots the vector specifies. The last
    /// slot is the aux digest, derived from ciphertext bytes the vector does not
    /// carry; `aux_digest_matches_the_sdk` pins it.
    #[test]
    fn the_challenge_layout_matches_the_published_vectors() {
        let v = vectors();
        for case in v["vectors"].as_array().expect("vectors") {
            let name = case["name"].as_str().unwrap_or("?");
            let w = &case["witness"];
            let expected = words(&case["compression"]["challenge"]);

            let pi = transact_from_witness(w);
            let aux = aux_from_witness(w);
            let got = challenge(&pi, &aux);
            assert_eq!(got.len(), expected.len(), "{name} length");

            for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
                if i == TRANSACT_CHALLENGE_WORDS - 1 {
                    continue; // auxDigest — see the doc comment above.
                }
                assert_eq!(g, e, "{name} word {i} ({})", slot_name(i));
            }
        }
    }

    /// Which field a mismatched slot belongs to, so a layout drift names itself
    /// rather than printing an index.
    fn slot_name(i: usize) -> &'static str {
        match i {
            0 => "merkleRoot",
            1..=4 => "nullifier",
            5..=10 => "outCm",
            11 => "publicAssetId",
            12 => "publicOut",
            13 => "digest",
            14 => "recipient",
            15 => "chainId",
            16 => "payer",
            17 => "relayer",
            18 => "intentHash",
            19..=36 => "clue",
            _ => "auxDigest",
        }
    }

    fn b32(s: &str) -> alloy::primitives::FixedBytes<32> {
        alloy::primitives::FixedBytes::<32>::from(u256(s).to_be_bytes::<32>())
    }

    fn addr(s: &str) -> alloy::primitives::Address {
        alloy::primitives::Address::from_slice(&u256(s).to_be_bytes::<32>()[12..])
    }

    fn transact_from_witness(w: &serde_json::Value) -> IMasp::Transact {
        let nf = strs(&w["nullifier"]);
        let cm = strs(&w["out_cm"]);
        IMasp::Transact {
            merkleRoot: b32(w["merkle_root"].as_str().unwrap()),
            nullifier: std::array::from_fn(|i| b32(&nf[i])),
            outCm: std::array::from_fn(|i| b32(&cm[i])),
            publicAssetId: w["public_asset_id"].as_str().unwrap().parse().unwrap(),
            publicOut: w["public_out"].as_str().unwrap().parse().unwrap(),
            digest: u256(w["digest"].as_str().unwrap()),
            recipient: addr(w["recipient_address"].as_str().unwrap()),
            chainId: u256(w["chain_id"].as_str().unwrap()),
            payer: addr(w["payer_address"].as_str().unwrap()),
            relayer: addr(w["relayer_address"].as_str().unwrap()),
            intentHash: u256(w["intent_hash"].as_str().unwrap()),
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
                clueQx: U256::ZERO,
                clueQy: U256::ZERO,
                ephPubX: U256::ZERO,
                ephPubY: U256::ZERO,
                ciphertext: b.to_be_bytes().to_vec().into(),
            }
        })
    }

    /// The aux digest covers the whole seven-field `AuxValidation.Output`, clue
    /// witness included. `tests/vectors/shielded-fee.json` carries six payloads
    /// the SDK built and `protocol/abi-hash.ts :: auxDigest` over them, in the
    /// order listed here.
    #[test]
    fn aux_digest_matches_the_sdk() {
        let f: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/vectors/shielded-fee.json"
        )))
        .expect("shielded-fee.json parses");
        let slots = [
            "fee",
            "feeSecond",
            "feeOtherAsset",
            "foreignOwner",
            "notOurs",
            "depositFee",
        ];
        let aux: [OutputAuxDto; TRANSACT_OUT] = slots.map(|slot| {
            serde_json::from_value(f[slot]["aux"].clone()).expect("aux is an OutputAuxDto")
        });
        assert_eq!(
            aux_digest(&build_aux(&aux).expect("aux builds")),
            u256(f["auxDigest"].as_str().expect("auxDigest"))
        );
    }

    /// Checked against the circuit's own declaration rather than a second
    /// hand-written number: the fixture publishes `coeffCount` and
    /// `challengeWords` alongside the vectors, so a drifted layout cannot be
    /// reconciled by editing a literal.
    #[test]
    fn the_spans_are_the_widths_the_circuit_declares() {
        let v = vectors();
        let declared = |key: &str| {
            v["circuit"][key]
                .as_u64()
                .unwrap_or_else(|| panic!("fixture declares {key}")) as usize
        };
        assert_eq!(TRANSACT_COEFFS, declared("coeffCount"));
        assert_eq!(TRANSACT_CHALLENGE_WORDS, declared("challengeWords"));
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
