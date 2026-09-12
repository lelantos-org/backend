//! Caching, rate-limiting JSON-RPC proxy for EVM reads.
//!
//! Serves browsers a reliable RPC endpoint while the paid upstream key stays
//! server-side. Three controls, applied in this order: rate limit, allowlist
//! (methods, and `eth_call` targets), cache with request coalescing.
//!
//! Reads only. Browser writes go through the user's wallet over EIP-1193 and do
//! not reach this service. Serving `eth_sendRawTransaction` would make a public
//! unauthenticated endpoint into an open relay billed to our provider account.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod services;

pub use app::build_info;
pub use app::{AppState, RpcProxyConfig, build_state};
pub use handlers::http::router::build as build_router;
