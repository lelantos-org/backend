//! Local Groth16 verification of the wallet's `transact_3x3` proof.
//!
//! Without this the first thing to check a wallet's proof is the contract —
//! after the relayer has already run a multi-second `tree_update_batch`
//! Groth16 behind a single-permit gate, holding the chain's tree mutex. Any
//! unauthenticated caller could therefore spend the relayer's prover on
//! payloads that were never going to land.
//!
//! Verification is a few pairings: milliseconds against seconds, and it runs
//! before the mirror lock is taken.

use crate::adapters::abi::IMasp;
use crate::domain::dto::{ProofDto, TRANSACT_OUT};
use crate::domain::error::{AppError, AppResult};
use crate::domain::transact_pi;
use ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine};
use ark_ec::short_weierstrass::{Affine, SWCurveConfig};
use ark_ff::PrimeField;
use ark_groth16::{Groth16, PreparedVerifyingKey, VerifyingKey};
use ark_snark::SNARK;
use serde::Deserialize;
use std::path::Path;
use std::str::FromStr;

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
    /// this circuit.
    fn into_verifying_key(self) -> AppResult<VerifyingKey<Bn254>> {
        if self.protocol != "groth16" || self.curve != "bn128" {
            return Err(AppError::Internal(format!(
                "transact vkey: expected groth16/bn128, got {}/{}",
                self.protocol, self.curve
            )));
        }
        // The circuit publishes exactly `(y, z)`; a key with any other arity
        // belongs to a different circuit and would verify nothing useful.
        if self.n_public != EXPECTED_PUBLIC_SIGNALS || self.ic.len() != EXPECTED_PUBLIC_SIGNALS + 1
        {
            return Err(AppError::Internal(format!(
                "transact vkey: expected {EXPECTED_PUBLIC_SIGNALS} public signals, got {} (IC len {})",
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
                .collect::<AppResult<Vec<_>>>()?,
        })
    }
}

/// `y` (the circuit's output) and `z` (the Fiat-Shamir challenge) — see
/// [`crate::domain::transact_pi`].
const EXPECTED_PUBLIC_SIGNALS: usize = 2;

pub struct TransactVerifier {
    pvk: PreparedVerifyingKey<Bn254>,
}

impl TransactVerifier {
    /// Load and prepare the deployed transact circuit's verification key.
    pub fn load(path: &Path) -> AppResult<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| AppError::Internal(format!("transact vkey {}: {e}", path.display())))?;
        let vk: SnarkjsVk = serde_json::from_str(&raw)
            .map_err(|e| AppError::Internal(format!("transact vkey {}: {e}", path.display())))?;
        Ok(Self {
            pvk: PreparedVerifyingKey::from(vk.into_verifying_key()?),
        })
    }

    /// Reject a payload whose transact proof does not verify against the
    /// public inputs it claims.
    pub fn verify(
        &self,
        proof: &ProofDto,
        pi: &IMasp::Transact,
        aux: &[IMasp::OutputAux; TRANSACT_OUT],
    ) -> AppResult<()> {
        let signals = transact_pi::compress(pi, aux);
        let public = [fr(signals.y), fr(signals.z)];
        let proof = ark_proof(proof)?;

        let ok = Groth16::<Bn254>::verify_with_processed_vk(&self.pvk, &public, &proof)
            .map_err(|e| AppError::Internal(format!("transact verify: {e}")))?;
        if !ok {
            return Err(AppError::BadRequest(
                "transact proof does not verify against its public inputs".into(),
            ));
        }
        Ok(())
    }
}

/// Wire proof → ark. The wire format is snarkjs-native, so the G2 coordinates
/// stay in `(c0, c1)` order here — the swap to `(c1, c0)` belongs only to the
/// Solidity verifier's calling convention, and `adapters::calldata` does it
/// there.
fn ark_proof(p: &ProofDto) -> AppResult<ark_groth16::Proof<Bn254>> {
    Ok(ark_groth16::Proof {
        a: g1(&p.pi_a, "piA", Origin::Payload)?,
        b: g2(&p.pi_b, "piB", Origin::Payload)?,
        c: g1(&p.pi_c, "piC", Origin::Payload)?,
    })
}

/// Whose fault a malformed curve point is.
///
/// The same parsing serves the verification key — a file this deployment ships
/// — and the proof, which is client input. A broken key is an operator problem
/// and must not be reported to a caller as a bad request.
#[derive(Debug, Clone, Copy)]
enum Origin {
    Vkey,
    Payload,
}

impl Origin {
    fn err(self, msg: String) -> AppError {
        match self {
            Origin::Vkey => AppError::Internal(format!("transact vkey: {msg}")),
            Origin::Payload => AppError::BadRequest(msg),
        }
    }
}

fn fq(s: &str, field: &str, origin: Origin) -> AppResult<Fq> {
    Fq::from_str(s).map_err(|_| origin.err(format!("{field}: not a BN254 base-field element: {s}")))
}

fn fr(v: alloy::primitives::U256) -> Fr {
    Fr::from_be_bytes_mod_order(&v.to_be_bytes::<32>())
}

/// Jacobian-ish snarkjs triple `[x, y, z]`, where `z == 0` is the point at
/// infinity and `z == 1` means the coordinates are already affine.
fn g1(p: &[String; 3], field: &str, origin: Origin) -> AppResult<G1Affine> {
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

fn g2(p: &[[String; 2]; 3], field: &str, origin: Origin) -> AppResult<G2Affine> {
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

/// A point off the curve or off the prime-order subgroup is not a proof
/// element. `new_unchecked` tests neither, and pairing an invalid point is
/// undefined rather than merely false.
fn check_on_curve<C: SWCurveConfig>(
    point: &Affine<C>,
    field: &str,
    origin: Origin,
) -> AppResult<()> {
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

    fn vkey_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../circuits/build/3x3_verification_key.json")
    }

    #[test]
    fn the_published_transact_vkey_loads() {
        let path = vkey_path();
        if !path.exists() {
            eprintln!("{} absent; skipping", path.display());
            return;
        }
        TransactVerifier::load(&path).expect("load published 3x3 vkey");
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
        let f = std::env::temp_dir().join("relayer_bad_arity_vk.json");
        std::fs::write(&f, serde_json::to_vec(&json).unwrap()).unwrap();
        let err = match TransactVerifier::load(&f) {
            Err(e) => e,
            Ok(_) => panic!("a 5-signal key is not this circuit's"),
        };
        assert!(err.to_string().contains("2 public signals"), "got {err}");
        // Operator problem, not the caller's.
        assert!(matches!(err, AppError::Internal(_)), "got {err}");
    }

    /// A broken verification key is a deployment fault. Reporting it as a
    /// `400` would blame whichever caller happened to arrive first.
    #[test]
    fn a_malformed_key_is_an_internal_error_not_a_bad_request() {
        let bad = ["1".into(), "1".into(), "1".into()];
        assert!(matches!(
            g1(&bad, "IC", Origin::Vkey).unwrap_err(),
            AppError::Internal(_)
        ));
        assert!(matches!(
            g1(&bad, "piA", Origin::Payload).unwrap_err(),
            AppError::BadRequest(_)
        ));
    }

    /// snarkjs writes the identity as `z = 0`; anything else non-affine is a
    /// malformed proof rather than something to renormalise.
    #[test]
    fn non_affine_points_are_rejected() {
        let err = g1(
            &["1".into(), "2".into(), "7".into()],
            "piA",
            Origin::Payload,
        )
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)), "got {err}");
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

    /// A point that parses as two field elements but is not on the curve must
    /// not reach the pairing.
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
