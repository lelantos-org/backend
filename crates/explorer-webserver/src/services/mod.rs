//! Orchestration: read a repository (or the asset registry), decorate with
//! prices, shape the response, and serve the whole thing from a cache.
//!
//! Nothing here names an axum type; handlers own that boundary.

pub mod anonymity_set;
pub mod asset_flows;
pub mod asset_yield;
pub mod assets;
pub mod locked;
pub mod pool_notes;
pub mod prices;
pub mod transactions;
pub mod tree_advances;

/// Serve a key from a cache, running the loader on a miss.
///
/// Every endpoint here has that shape, and so does every other webserver in the
/// workspace, so the helper lives in `shared::cache` rather than being restated
/// per crate. This crate's values are all `Arc<T>`, which is the `V: Clone` the
/// helper wants.
pub(crate) use shared::cache::cached;
