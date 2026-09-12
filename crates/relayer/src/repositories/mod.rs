//! Layer 4: database reads, one module per aggregate.
//!
//! The relayer owns no tables. Every module here reads something an indexer
//! wrote: the tree frontier, the notes it replays from, the spent nullifiers it
//! checks against, and the asset catalog.

pub mod assets;
pub mod notes;
pub mod spent_nullifiers;
pub mod tree_advances;
pub mod tree_state;

use crate::domain::error::{AppError, AppResult};
use database::{DbConn, DbPool};

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`AppError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> AppResult<DbConn<'_>> {
    pool.get().await.map_err(|e| AppError::Db(e.to_string()))
}
