//! Database I/O for the rows this service owns.
//!
//! Writes to two tables' worth: the estimate columns on `asset_yield` and the
//! whole of `asset_yield_sample`. Everything else the catalog serves is read
//! through `asset-registry`, which owns that join. [`gov_proposals`] and
//! [`gov_votes`] only read, from tables protocol-indexer writes.

pub mod asset_yield;
pub mod gov_proposals;
pub mod gov_votes;
pub mod yield_samples;

use crate::domain::error::{AppError, AppResult};
use database::{DbConn, DbPool};
use std::fmt::Display;

/// A position in a newest-first log: `(block_number, log_index)` of the last
/// row a page returned.
///
/// The governance reads are keyset-paginated on it: both tables are
/// append-mostly logs read from the head, and an offset would rescan every row
/// a client has already paged past.
pub type Position = (i64, i32);

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`AppError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> AppResult<DbConn<'_>> {
    pool.get().await.map_err(db_err)
}

/// Restate a driver failure as [`AppError::Db`].
///
/// The driver string names tables, columns and sometimes the failing value, so
/// it is carried for the log and never for the body; `shared::http` is what
/// enforces that. Written once here so no query site can quietly decide
/// otherwise.
pub(crate) fn db_err(e: impl Display) -> AppError {
    AppError::Db(e.to_string())
}
