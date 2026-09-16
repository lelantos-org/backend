//! Protocol indexer.
//!
//! Projects public chain events into the asset catalog, the yield bindings and
//! their polled index, the Merkle advance log and the deposit ledger.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Owns two ticking services
//! (`ConsumeServiceImpl`, `YieldStateServiceImpl`) implementing
//! `shared::tick::TickService`. Must not depend on `crypto`, which is
//! the privacy gate.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod repositories;
pub mod services;
