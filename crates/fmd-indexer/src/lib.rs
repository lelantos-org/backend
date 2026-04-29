//! FMD indexer.
//!
//! Layered binary; see `backend/ARCHITECTURE.md` for the crate-wide rules.
//! Owns two ticking services (`ConsumeServiceImpl`, `FilterServiceImpl`)
//! both implementing `shared::tick::TickService`.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;
