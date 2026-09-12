//! Database reads, one module per aggregate.
//!
//! Returns rows, never response types, and never reaches into `services/`,
//! `handlers/` or `adapters/`.

pub mod anonymity_set;
pub mod asset_flows;
pub mod asset_locked;
pub mod asset_yield;
pub mod chains;
pub mod pool_notes;
pub mod transactions;
pub mod tree_advances;

use crate::domain::error::{AppError, AppResult};
use database::{DbConn, DbPool};

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`AppError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> AppResult<DbConn<'_>> {
    pool.get().await.map_err(|e| AppError::Db(e.to_string()))
}

/// Restate a failed query as [`AppError::Db`].
///
/// Written once so every read reports a driver failure the same way: the detail
/// names tables, columns and sometimes the failing value, and `shared::http`
/// logs it rather than returning it.
pub(crate) fn db_err(e: diesel::result::Error) -> AppError {
    AppError::Db(e.to_string())
}
