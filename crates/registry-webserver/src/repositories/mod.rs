//! Database I/O for the rows this service owns.
//!
//! Two tables' worth: the estimate columns on `asset_yield` and the whole of
//! `asset_yield_sample`. Everything else the catalog serves is read through
//! `asset-registry`, which owns that join.

pub mod asset_yield;
pub mod yield_samples;

use crate::domain::error::{AppError, AppResult};
use database::{DbConn, DbPool};
use std::fmt::Display;

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
