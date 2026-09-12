//! Local Groth16 verification of the wallet's transact proof.
//!
//! Without it the first check of a wallet's proof happens on chain, after the
//! relayer has run a multi-second `tree_update_batch` Groth16 behind a
//! single-permit gate while holding the chain's tree mutex, letting any
//! unauthenticated caller consume the prover on payloads that cannot land.
//!
//! Verification is a few pairings, milliseconds against seconds, and it runs
//! before the mirror lock is taken.
//!
//! The pairings themselves live in [`groth16`], which knows nothing of this
//! circuit. What is relayer-specific is here: which public signals the deployed
//! transact shape publishes, and how they are derived from the payload.

use crate::adapters::abi::IMasp;
use crate::domain::dto::{ProofDto, TRANSACT_OUT};
use crate::domain::error::AppResult;
use crate::domain::transact_pi;
use groth16::{Groth16Verifier, SnarkjsProof};
use std::path::Path;

/// `y`, the circuit's output, and `z`, the Fiat-Shamir challenge. See
/// [`crate::domain::transact_pi`].
const EXPECTED_PUBLIC_SIGNALS: usize = 2;

pub struct TransactVerifier {
    inner: Groth16Verifier,
}

impl TransactVerifier {
    /// Load and prepare the deployed transact circuit's verification key.
    pub fn load(path: &Path) -> AppResult<Self> {
        Ok(Self {
            inner: Groth16Verifier::load(path, EXPECTED_PUBLIC_SIGNALS)?,
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
        // Big-endian words in the order the circuit declares them, which is the
        // order the deployed verifier is handed its two public signals.
        let public = [signals.y.to_be_bytes::<32>(), signals.z.to_be_bytes::<32>()];
        self.inner.verify(
            SnarkjsProof {
                pi_a: &proof.pi_a,
                pi_b: &proof.pi_b,
                pi_c: &proof.pi_c,
            },
            &public,
        )?;
        Ok(())
    }
}
