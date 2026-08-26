//! Cross-crate shared types and runtime primitives.
//!
//! Everything here is consumed by the indexers, the webservers and the relayer.
//! No domain logic belongs here, only:
//!   - shared entities (event kinds, chain ids)
//!   - runtime helpers (shutdown, tick driver, config loader, tracing init)
//!
//! Layering:
//!   - May import: nothing internal; this is the bottom of the stack.
//!   - Must not import: `database`, any binary, any service crate.

pub mod backoff;
pub mod build_info;
pub mod chain;
pub mod config;
pub mod config_env;
pub mod entities;
pub mod metrics;
pub mod shutdown;
pub mod tick;
pub mod tracing_init;

/// Re-exported so [`build_info!`] can emit its banner without the calling crate
/// having to name `tracing` itself.
pub use tracing;

#[cfg(feature = "webserver")]
pub mod cache;
#[cfg(feature = "webserver")]
pub mod http;
#[cfg(feature = "webserver")]
pub mod request_span;
