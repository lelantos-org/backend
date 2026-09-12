//! Groth16 verification against a snarkjs `verification_key.json`.
//!
//! Circuit-agnostic on purpose. The caller derives its own public signals —
//! whatever its circuit publishes — and hands them over as big-endian 32-byte
//! words, so nothing about a particular circuit, and no arkworks type, crosses
//! this boundary.
//!
//! The relayer's use is checking a wallet's transact proof before the tree-update
//! prove. Without it the first check of that proof happens on chain, after a
//! multi-second `tree_update_batch` Groth16 has run behind a single-permit gate
//! while holding the chain's tree mutex, letting any unauthenticated caller
//! consume the prover on payloads that cannot land. Verification is a few
//! pairings, milliseconds against seconds.

use crate::error::{Groth16Error, Groth16Result};
use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ec::short_weierstrass::{Affine, SWCurveConfig};
use ark_ff::PrimeField;
use ark_groth16::{Groth16, PreparedVerifyingKey, VerifyingKey};
use ark_snark::SNARK;
use num_bigint::BigUint;
use serde::Deserialize;
use std::path::Path;
use std::str::FromStr;

/// A Groth16 proof in snarkjs' own wire shape: decimal coordinate strings, with
/// the trailing element of each point carrying the projective `z`.
///
/// Taken as strings rather than as parsed points because that is how a proof
/// arrives — out of JSON, from a wallet — and parsing it is exactly the step
/// that must distinguish a caller's malformed input from an operator's broken
/// key. The G2 coordinates stay in snarkjs' `(c0, c1)` order; the swap to
/// `(c1, c0)` belongs to the Solidity verifier's calling convention and is the
/// caller's business.
///
/// Borrows, so a caller whose own wire type already holds these arrays points at
/// them rather than cloning ten strings per verification.
#[derive(Debug, Clone, Copy)]
pub struct SnarkjsProof<'a> {
    pub pi_a: &'a [String; 3],
    pub pi_b: &'a [[String; 2]; 3],
    pub pi_c: &'a [String; 3],
}

/// snarkjs `verification_key.json`, only the fields a verifier needs.
#[derive(Debug, Deserialize)]
struct SnarkjsVk {
    protocol: String,
    curve: String,
    #[serde(rename = "nPublic")]
    n_public: usize,
    vk_alpha_1: [String; 3],
    vk_beta_2: [[String; 2]; 3],
    vk_gamma_2: [[String; 2]; 3],
    vk_delta_2: [[String; 2]; 3],
    #[serde(rename = "IC")]
    ic: Vec<[String; 3]>,
}

impl SnarkjsVk {
    /// Convert to ark's representation, rejecting a key that does not describe
    /// the circuit the caller asked for.
    fn into_verifying_key(self, public_signals: usize) -> Groth16Result<VerifyingKey<Bn254>> {
        if self.protocol != "groth16" || self.curve != "bn128" {
            return Err(Groth16Error::Key(format!(
                "expected groth16/bn128, got {}/{}",
                self.protocol, self.curve
            )));
        }
        // Arity is what tells one circuit's key from another's, and it is the
        // only property of the caller's circuit this crate knows. A key with the
        // wrong one would otherwise fail later as an unverifiable proof, which
        // reads as the caller's fault rather than the deployment's.
        if self.n_public != public_signals || self.ic.len() != public_signals + 1 {
            return Err(Groth16Error::Key(format!(
                "expected {public_signals} public signals, got {} (IC len {})",
                self.n_public,
                self.ic.len()
            )));
        }
        Ok(VerifyingKey {
            alpha_g1: g1(&self.vk_alpha_1, "vk_alpha_1", Origin::Vkey)?,
            beta_g2: g2(&self.vk_beta_2, "vk_beta_2", Origin::Vkey)?,
            gamma_g2: g2(&self.vk_gamma_2, "vk_gamma_2", Origin::Vkey)?,
            delta_g2: g2(&self.vk_delta_2, "vk_delta_2", Origin::Vkey)?,
            gamma_abc_g1: self
                .ic
                .iter()
                .map(|p| g1(p, "IC", Origin::Vkey))
                .collect::<Groth16Result<Vec<_>>>()?,
        })
    }
}

/// A prepared verification key for one circuit, and the public-signal arity it
/// was loaded for.
pub struct Groth16Verifier {
    pvk: PreparedVerifyingKey<Bn254>,
    public_signals: usize,
}

impl Groth16Verifier {
    /// Load and prepare a snarkjs verification key, refusing one that does not
    /// publish `public_signals` signals.
    ///
    /// Preparation is the expensive half of verifying — it precomputes the
    /// pairing inputs that do not depend on the proof — so a caller loads once
    /// and keeps the verifier for the process lifetime.
    pub fn load(path: &Path, public_signals: usize) -> Groth16Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Groth16Error::Key(format!("{}: {e}", path.display())))?;
        let vk: SnarkjsVk = serde_json::from_str(&raw)
            .map_err(|e| Groth16Error::Key(format!("{}: {e}", path.display())))?;
        Ok(Self {
            pvk: PreparedVerifyingKey::from(vk.into_verifying_key(public_signals)?),
            public_signals,
        })
    }

    /// Reject a proof that does not verify against the public inputs it claims.
    ///
    /// `public` holds one big-endian 32-byte word per public signal, in the
    /// order the circuit declares them. Words are reduced modulo the scalar
    /// field rather than refused for being oversized: they are the caller's own
    /// derived signals, not attacker-chosen field elements, and every source of
    /// them already works modulo `r`.
    pub fn verify(&self, proof: SnarkjsProof<'_>, public: &[[u8; 32]]) -> Groth16Result<()> {
        if public.len() != self.public_signals {
            return Err(Groth16Error::Verify(format!(
                "key publishes {} signals, got {}",
                self.public_signals,
                public.len()
            )));
        }
        let public: Vec<Fr> = public
            .iter()
            .map(|w| Fr::from_be_bytes_mod_order(w))
            .collect();
        let proof = ark_proof(proof)?;

        let ok = Groth16::<Bn254>::verify_with_processed_vk(&self.pvk, &public, &proof)
            .map_err(|e| Groth16Error::Verify(e.to_string()))?;
        if !ok {
            return Err(Groth16Error::InvalidProof(
                "proof does not verify against its public inputs".into(),
            ));
        }
        Ok(())
    }
}

/// Wire proof to ark.
fn ark_proof(p: SnarkjsProof<'_>) -> Groth16Result<ark_groth16::Proof<Bn254>> {
    Ok(ark_groth16::Proof {
        a: g1(p.pi_a, "piA", Origin::Payload)?,
        b: g2(p.pi_b, "piB", Origin::Payload)?,
        c: g1(p.pi_c, "piC", Origin::Payload)?,
    })
}

/// Whose fault a malformed curve point is.
///
/// The same parsing serves the verification key, a file this deployment ships,
/// and the proof, which is client input. A broken key is an operator problem and
/// must not be reported to a caller as a bad request.
#[derive(Debug, Clone, Copy)]
enum Origin {
    Vkey,
    Payload,
}

impl Origin {
    fn err(self, msg: String) -> Groth16Error {
        match self {
            Origin::Vkey => Groth16Error::Key(msg),
            Origin::Payload => Groth16Error::InvalidProof(msg),
        }
    }
}

/// Parse one decimal base-field coordinate, canonically.
///
/// Not `Fq::from_str`, which accumulates in the field and so silently reduces:
/// it maps `q` to 0, `q + 7` to 7 and even `"-1"` to `q - 1`, all reported as
/// success. Every such spelling decodes to a point that verifies exactly as its
/// canonical form does, so the effect is proof-encoding malleability rather than
/// a soundness break -- but the error below promises a BN254 base-field element,
/// and a parse that reduces does not deliver one.
fn fq(s: &str, field: &str, origin: Origin) -> Groth16Result<Fq> {
    let reject = || origin.err(format!("{field}: not a BN254 base-field element: {s}"));
    // `BigUint` rather than `BigInt`: it refuses a sign, so a negative spelling
    // fails here rather than wrapping into the field.
    let n = BigUint::from_str(s).map_err(|_| reject())?;
    let bigint = <Fq as PrimeField>::BigInt::try_from(n).map_err(|_| reject())?;
    if bigint >= Fq::MODULUS {
        return Err(reject());
    }
    Fq::from_bigint(bigint).ok_or_else(reject)
}

/// Jacobian-ish snarkjs triple `[x, y, z]`, where `z == 0` is the point at
/// infinity and `z == 1` means the coordinates are already affine.
fn g1(p: &[String; 3], field: &str, origin: Origin) -> Groth16Result<G1Affine> {
    if p[2] == "0" {
        return Ok(G1Affine::identity());
    }
    if p[2] != "1" {
        return Err(origin.err(format!(
            "{field}: expected an affine point (z == 1), got z = {}",
            p[2]
        )));
    }
    let point = G1Affine::new_unchecked(fq(&p[0], field, origin)?, fq(&p[1], field, origin)?);
    check_on_curve(&point, field, origin)?;
    Ok(point)
}

fn g2(p: &[[String; 2]; 3], field: &str, origin: Origin) -> Groth16Result<G2Affine> {
    if p[2][0] == "0" && p[2][1] == "0" {
        return Ok(G2Affine::identity());
    }
    if p[2][0] != "1" || p[2][1] != "0" {
        return Err(origin.err(format!(
            "{field}: expected an affine point (z == [1, 0]), got z = [{}, {}]",
            p[2][0], p[2][1]
        )));
    }
    let point = G2Affine::new_unchecked(
        Fq2::new(fq(&p[0][0], field, origin)?, fq(&p[0][1], field, origin)?),
        Fq2::new(fq(&p[1][0], field, origin)?, fq(&p[1][1], field, origin)?),
    );
    check_on_curve(&point, field, origin)?;
    Ok(point)
}

/// A point off the curve or outside the prime-order subgroup is not a proof
/// element. `new_unchecked` tests neither, and pairing an invalid point is
/// undefined rather than false.
fn check_on_curve<C: SWCurveConfig>(
    point: &Affine<C>,
    field: &str,
    origin: Origin,
) -> Groth16Result<()> {
    if point.is_on_curve() && point.is_in_correct_subgroup_assuming_on_curve() {
        return Ok(());
    }
    Err(origin.err(format!(
        "{field}: point is not on the BN254 curve in the correct subgroup"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ec::AffineRepr;

    /// The transact circuit publishes `(y, z)`: its output and the Fiat-Shamir
    /// challenge. The relayer names the same number where it loads the key.
    const TRANSACT_SIGNALS: usize = 2;

    fn vkey_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../circuits/build/4x6_verification_key.json")
    }

    #[test]
    fn the_published_transact_vkey_loads() {
        let path = vkey_path();
        if !path.exists() {
            eprintln!("{} absent; skipping", path.display());
            return;
        }
        Groth16Verifier::load(&path, TRANSACT_SIGNALS).expect("load published 4x6 vkey");
    }

    #[test]
    fn a_vkey_with_the_wrong_arity_is_refused() {
        let json = serde_json::json!({
            "protocol": "groth16",
            "curve": "bn128",
            "nPublic": 5,
            "vk_alpha_1": ["1", "2", "1"],
            "vk_beta_2": [["1", "2"], ["3", "4"], ["1", "0"]],
            "vk_gamma_2": [["1", "2"], ["3", "4"], ["1", "0"]],
            "vk_delta_2": [["1", "2"], ["3", "4"], ["1", "0"]],
            "IC": [["1", "2", "1"]],
        });
        let f = std::env::temp_dir().join("groth16_bad_arity_vk.json");
        std::fs::write(&f, serde_json::to_vec(&json).unwrap()).unwrap();
        let err = match Groth16Verifier::load(&f, TRANSACT_SIGNALS) {
            Err(e) => e,
            Ok(_) => panic!("a 5-signal key is not this circuit's"),
        };
        assert!(err.to_string().contains("2 public signals"), "got {err}");
        // An operator problem rather than the caller's.
        assert!(matches!(err, Groth16Error::Key(_)), "got {err}");
    }

    /// A broken verification key is a deployment fault; reporting it the way a
    /// bad proof is reported would blame whichever caller arrived first.
    #[test]
    fn a_malformed_key_is_not_reported_as_a_malformed_proof() {
        let bad = ["1".into(), "1".into(), "1".into()];
        assert!(matches!(
            g1(&bad, "IC", Origin::Vkey).unwrap_err(),
            Groth16Error::Key(_)
        ));
        assert!(matches!(
            g1(&bad, "piA", Origin::Payload).unwrap_err(),
            Groth16Error::InvalidProof(_)
        ));
    }

    /// snarkjs writes the identity as `z = 0`; any other non-affine value is a
    /// malformed proof rather than something to renormalise.
    #[test]
    fn non_affine_points_are_rejected() {
        let err = g1(
            &["1".into(), "2".into(), "7".into()],
            "piA",
            Origin::Payload,
        )
        .unwrap_err();
        assert!(matches!(err, Groth16Error::InvalidProof(_)), "got {err}");
        assert!(
            g1(
                &["0".into(), "1".into(), "0".into()],
                "piA",
                Origin::Payload
            )
            .unwrap()
            .is_zero()
        );
    }

    /// `Fq::from_str` accepts all three of these and reduces them into the
    /// field, so a proof has many spellings that decode to the same point. The
    /// parse has to refuse them for the error it returns to be true.
    #[test]
    fn non_canonical_coordinates_are_rejected() {
        let q = <Fq as PrimeField>::MODULUS.to_string();
        let q_plus_7 = (BigUint::from_str(&q).unwrap() + 7u8).to_string();
        for spelling in [q.as_str(), q_plus_7.as_str(), "-1"] {
            let p = [spelling.to_string(), "2".into(), "1".into()];
            let err = match g1(&p, "piA", Origin::Payload) {
                Err(e) => e,
                Ok(_) => panic!("{spelling} is not a canonical base-field element"),
            };
            assert!(
                err.to_string().contains("not a BN254 base-field element"),
                "{spelling}: got {err}"
            );
        }
    }

    /// The largest canonical coordinate must still parse: a bound written one
    /// off would refuse a legitimate proof, which is the worse failure. Derived
    /// from the field rather than written out, so it tracks BN254.
    #[test]
    fn the_largest_canonical_coordinate_is_accepted() {
        let max = -Fq::from(1u8);
        let decimal = max.into_bigint().to_string();
        assert_eq!(fq(&decimal, "piA", Origin::Payload).unwrap(), max);
    }

    /// A point that parses as two field elements but is not on the curve must not
    /// reach the pairing.
    #[test]
    fn off_curve_points_are_rejected() {
        let err = g1(
            &["1".into(), "1".into(), "1".into()],
            "piA",
            Origin::Payload,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not on the BN254 curve"),
            "got {err}"
        );
    }
}
