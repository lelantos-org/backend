//! Cached view of the `assets` table.
//!
//! Moved to the `asset-registry` crate, shared with the catalog service.
//! Re-exported here so this crate's `services::asset_registry` path keeps
//! meaning what it did.

pub use ::asset_registry::AssetRegistry;
