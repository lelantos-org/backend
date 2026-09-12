//! The `assets` ⋈ `asset_yield` read.
//!
//! Moved to the `asset-registry` crate, shared with the catalog service so both
//! sides join the same columns into the same row. Re-exported here so this
//! crate's `repositories::assets` path keeps meaning what it did.

pub use ::asset_registry::{AssetRow, list_for_chain, list_for_chains};
