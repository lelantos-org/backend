//! Tree-update relayer.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns the tree mirror, fee
//! quoting and on-chain submission; the Groth16 itself — proving over a snarkjs
//! zkey, and verifying a wallet's transact proof — lives in the `groth16` crate,
//! which serialises CPU-heavy proofs behind its own `Semaphore`.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::{AppState, RelayerConfig, build_state};
pub use handlers::http::router::build as build_router;
