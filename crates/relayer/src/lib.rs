//! Tree-update relayer.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns proof generation
//! (native Groth16 over a snarkjs zkey, witness from a circom-witnesscalc graph)
//! and on-chain submission. The prover serialises CPU-heavy proofs behind its own
//! `Semaphore`; see `services::prover`.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::{AppState, RelayerConfig, build_state};
pub use handlers::http::router::build as build_router;
