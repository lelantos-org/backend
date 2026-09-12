//! Layer 1: configuration, app state and build stamp. No business logic.

pub mod build_info;
pub mod config;
pub mod state;

pub use config::RelayerConfig;
pub use state::{AppState, build_state};
