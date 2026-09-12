//! Native Groth16 over BN254, against snarkjs-produced artifacts.
//!
//! Everything the relayer needs to prove `tree_update_batch` and to verify a
//! wallet's `transact` proof, with the arkworks 0.6 stack, the vendored snarkjs
//! zkey parser and the circom witness-graph evaluator confined here rather than
//! spread through a binary. Nothing in this crate reaches for a database, an RPC
//! endpoint or an HTTP framework, and its errors are its own
//! ([`Groth16Error`]); a binary maps them onto whatever it reports.
//!
//! ```text
//! witness_calc  WitnessCalculator   circom graph -> full witness
//! zkey          read_zkey           snarkjs zkey -> ProvingKey + matrices
//! prover        Groth16Prover       witness -> proof, serialised behind a permit
//! verifier      Groth16Verifier     snarkjs vkey + proof + public inputs -> ok
//! ```
//!
//! The proving side is circuit-specific: [`TreeUpdateBatchWitness`] names
//! `tree_update_batch.circom`'s signals, and the caller composes it. The
//! verifying side is not — [`Groth16Verifier`] takes any snarkjs verification
//! key and checks a proof against public inputs given as big-endian 32-byte
//! words, so a caller derives its own public signals however its circuit
//! defines them.
//!
//! Nothing in the public API names an arkworks or `num-bigint` type. Witnesses
//! and proofs cross as decimal strings, public inputs as `[u8; 32]`, and the
//! field arithmetic stays inside.

mod error;
mod prover;
/// The snarkjs R1CS-to-QAP reduction, kept as a test oracle.
///
/// `taceo-groth16` ships its own `CircomReduction` and the prover uses that one.
/// This is the independent second implementation `prover`'s `measure_phase_split`
/// checks it against: two routes to the same proof is a far stronger statement
/// than either one verifying alone. It is not compiled into the library.
#[cfg(test)]
mod qap;
mod verifier;
mod witness_calc;
mod zkey;

pub use error::{Groth16Error, Groth16Result};
pub use prover::{
    Groth16Prover, Priority, TreeUpdateBatchProof, TreeUpdateBatchProver, TreeUpdateBatchWitness,
};
pub use verifier::{Groth16Verifier, SnarkjsProof};
