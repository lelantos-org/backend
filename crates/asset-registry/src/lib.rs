//! The asset catalog: the `assets` ⋈ `asset_yield` join, its circuit-unit
//! arithmetic, and a cached read of it.
//!
//! `protocol-indexer` writes these tables; every other service reads them
//! through here. A leaf library rather than a shared binary because
//! `backend/ARCHITECTURE.md` forbids one binary importing another, and three
//! crates need the same row shape: the relayer (fee decoration and shielded-fee
//! admission), `registry-webserver` (which serves the catalog) and
//! `explorer-webserver` (asset-backed analytics).

pub mod cache;
pub mod error;
pub mod numeric;
pub mod repo;
pub mod row;
pub mod units;

pub use cache::AssetRegistry;
pub use error::{Error, Result};
pub use numeric::bigdecimal_to_u256;
pub use repo::{list_all, list_for_chain, list_for_chains};
pub use row::{ApyEstimate, AssetRow};
pub use units::{Rate, Scale};
