//! Tree-update relayer.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns proof generation
//! (Groth16 via ark-circom) and on-chain submission. Per-chain pipelines gate the
//! prover behind a `parking_lot::Mutex` to serialise CPU-heavy proofs.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::{AppState, RelayerConfig, build_state};
pub use handlers::http::router::build as build_router;
