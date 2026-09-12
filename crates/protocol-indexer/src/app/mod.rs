//! Process-level wiring: the TOML config and the build identity.
//!
//! No business logic; see `backend/ARCHITECTURE.md`.

pub mod build_info;
pub mod config;

pub use config::ProtocolIndexerConfig;
