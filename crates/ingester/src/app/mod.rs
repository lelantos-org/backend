//! Config, wiring and per-chain state. No business logic.

pub mod config;
pub mod state;

pub use config::{ChainConfig, IngesterConfig};
pub use state::WorkerDeps;
