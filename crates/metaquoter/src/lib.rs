//! Quote-aggregation backend for shielded swaps.
//!
//! Layered binary; see `backend/ARCHITECTURE.md`. Races the venue-specific
//! [`Quoter`](repositories::quoter::Quoter) impls per request and returns the
//! route with the highest `expected_out`. UniV3 is the only venue implemented;
//! further venues plug in through the same trait.

pub mod adapters;
pub mod app;
pub mod domain;
pub mod handlers;
pub mod repositories;
pub mod services;

pub use app::{AppState, MetaQuoterConfig, build_state};
pub use handlers::http::router::build as build_router;
