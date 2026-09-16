//! Layer 4: database I/O, one module per aggregate.
//!
//! The relayer owns no tables. Every module here reads something an indexer
//! wrote: the tree frontier, the notes it replays from, the spent nullifiers it
//! checks against and the escrowed-deposit ledger it flushes from. The one write
//! is the optimistic flush mark on that ledger, which the indexer later
//! overwrites. The asset catalog is read through the `asset-registry` crate.

pub mod deposit_escrowed_events;
pub mod notes;
pub mod numeric;
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
