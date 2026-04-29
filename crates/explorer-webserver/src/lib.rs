//! Explorer webserver.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Read-only HTTP API for
//! assets, asset flows, tree advances. Error type from `shared::http`.
//! Must NOT depend on `fmd-crypto` (privacy gate).

pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::build_info;
pub use app::{AppState, ExplorerWebserverConfig};
pub use handlers::http::router::build as build_router;
