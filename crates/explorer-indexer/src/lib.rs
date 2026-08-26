//! Explorer indexer.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns one ticking service
//! (`ConsumeServiceImpl`) implementing `shared::tick::TickService`. Must not
//! depend on `fmd-crypto`, which is the privacy gate.

pub mod adapters;
pub mod build_info;
pub mod config;
pub mod error;
pub mod repositories;
pub mod services;
pub mod util;
