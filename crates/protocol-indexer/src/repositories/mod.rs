//! Database I/O, one module per table this crate writes.
//!
//! Each function takes the pool and returns rows; the orchestration that
//! decides what to write lives in `services`.

pub mod asset_yield;
pub mod assets;
pub mod cursor;
pub mod deposit_events;
pub mod tree_advances;

use crate::domain::error::ProtocolIndexerError;
use database::{DbConn, DbPool};

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`ProtocolIndexerError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> Result<DbConn<'_>, ProtocolIndexerError> {
    pool.get()
        .await
        .map_err(|e| ProtocolIndexerError::Db(e.to_string()))
}
