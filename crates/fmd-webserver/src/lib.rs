//! FMD webserver.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Read-only HTTP API for
//! notes, matches, subscriptions, and tree proofs. Error type comes from
//! `shared::http`.

pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::build_info;
pub use app::{AppState, FmdWebserverConfig};
pub use handlers::http::router::build as build_router;
