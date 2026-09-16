//! Database I/O, one module per table this crate writes.
//!
//! Each function takes the pool and returns rows; the orchestration that
//! decides what to write lives in `services`.

//!
//! The `consumer_cursors` row is read and advanced through `database`'s shared
//! `CursorRepo`, per `ARCHITECTURE.md`.

pub mod asset_yield;
pub mod assets;
pub mod deposit_escrowed_events;
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
