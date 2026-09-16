//! Explorer webserver.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Read-only HTTP API over the
//! tables `explorer-indexer` writes: the asset catalog, flows, escrowed
//! balances, tree advances, classified transactions, withdrawal anonymity sets,
//! pool occupancy and yield state. Errors come from `shared::http`.
//!
//! Must not depend on `crypto`, which is the privacy gate. It is a
//! convention, not a CI check, so a new dependency edge has to be caught in
//! review.

pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::build_info;
pub use app::{AppState, ExplorerWebserverConfig};
pub use handlers::http::router::build as build_router;
