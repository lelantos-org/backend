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

pub mod public_signals;

use crate::adapters::abi::IMasp;
use crate::domain::dto::{ProofDto, TRANSACT_OUT};
use crate::domain::error::AppResult;
use groth16::{Groth16Verifier, SnarkjsProof};
use public_signals::TransactPublicSignals;
use std::path::Path;

pub struct TransactVerifier {
    inner: Groth16Verifier,
}

impl TransactVerifier {
    /// Load and prepare the deployed transact circuit's verification key.
    pub fn load(path: &Path) -> AppResult<Self> {
        Ok(Self {
            inner: Groth16Verifier::load(path, TransactPublicSignals::COUNT)?,
        })
    }

    /// Reject a payload whose transact proof does not verify against the
    /// public inputs it claims.
    ///
    /// `pi.digest` must already be a canonical field element
    /// (`pipeline::transact::check_field_elements`): the verifier reduces its
    /// public words, where the on-chain verifier rejects one outside the field.
    pub fn verify(
        &self,
        proof: &ProofDto,
        pi: &IMasp::Transact,
        aux: &[IMasp::OutputAux; TRANSACT_OUT],
    ) -> AppResult<()> {
        let signals = public_signals::compress(pi, aux);
        self.inner.verify(
            SnarkjsProof {
                pi_a: &proof.pi_a,
                pi_b: &proof.pi_b,
                pi_c: &proof.pi_c,
            },
            &signals.words(),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key `just fetch-circuits` pins for the stack publishes
    /// [`TransactPublicSignals::COUNT`] signals.
    #[test]
    fn the_published_transact_vkey_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stack/circuits/4x6_verification_key.json");
        if !path.exists() {
            eprintln!("{} absent; skipping", path.display());
            return;
        }
        if let Err(e) = TransactVerifier::load(&path) {
            panic!("load published 4x6 vkey: {e}");
        }
    }
}
