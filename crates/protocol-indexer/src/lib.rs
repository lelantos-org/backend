//! Protocol indexer.
//!
//! Projects public chain events into the asset catalog, the yield bindings and
//! their polled index, the Merkle advance log and the deposit ledger.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns two ticking services
//! (`ConsumeServiceImpl`, `YieldStateServiceImpl`) implementing
//! `shared::tick::TickService`. Must not depend on `common-crypto`, which is
//! the privacy gate.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod repositories;
pub mod services;

// The three modules above used to sit at the crate root. Re-exported so the old
// paths keep resolving.
pub use app::{build_info, config};
pub use domain::error;
