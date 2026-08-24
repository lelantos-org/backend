//! Cross-crate shared types and runtime primitives.
//!
//! Everything here is consumed by indexers, webservers, and the relayer.
//! Do NOT add domain logic. Add only:
//!   - shared entities (event kinds, chain ids)
//!   - runtime helpers (shutdown, tick driver, config loader, tracing init)
//!
//! Layering:
//!   - May import: nothing internal (this is the bottom of the stack).
//!   - Must NOT import: `database`, any binary, any service crate.

pub mod backoff;
pub mod chain;
pub mod config;
pub mod config_env;
pub mod entities;
pub mod metrics;
pub mod shutdown;
pub mod tick;
pub mod tracing_init;

#[cfg(feature = "webserver")]
pub mod cache;
#[cfg(feature = "webserver")]
pub mod http;
#[cfg(feature = "webserver")]
pub mod request_span;
