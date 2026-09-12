//! Config, shared state and build identity. No business logic; see
//! `backend/ARCHITECTURE.md`.

pub mod build_info;
pub mod cache;
pub mod config;
pub mod state;

pub use config::FmdWebserverConfig;
pub use state::AppState;
