//! Quote-aggregation backend for shielded swaps.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Races venue-specific
//! [`Quoter`](repositories::quoter::Quoter) impls per request and returns
//! the highest-`expected_out` route. Phase 1 ships UniV3 only; Curve and
//! 1inch slot in via the same trait.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::{AppState, MetaQuoterConfig, build_state};
pub use handlers::http::router::build as build_router;
