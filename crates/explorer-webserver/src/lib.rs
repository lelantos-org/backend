//! Explorer webserver.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Read-only HTTP API for assets,
//! asset flows and tree advances, using the error type from `shared::http`. Must
//! not depend on `fmd-crypto`, which is the privacy gate.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::build_info;
pub use app::{AppState, ExplorerWebserverConfig};
pub use handlers::http::router::build as build_router;
