//! Explorer indexer.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns one ticking
//! service (`ConsumeServiceImpl`) implementing `shared::tick::TickService`.
//! Must NOT depend on `fmd-crypto` (privacy gate).

pub mod config;
pub mod error;
pub mod repositories;
pub mod services;
pub mod util;
pub mod version;
