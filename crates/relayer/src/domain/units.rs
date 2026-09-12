//! Circuit units and ERC-20 base units.
//!
//! Moved to the `asset-registry` crate, which owns the asset row these operate
//! on and is shared with the catalog service. Re-exported here so this crate's
//! `domain::units` path keeps meaning what it did.

pub use ::asset_registry::{Rate, Scale};
