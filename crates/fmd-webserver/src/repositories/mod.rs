//! Database I/O, one module per aggregate. Returns rows; no orchestration and no
//! HTTP types. See `backend/ARCHITECTURE.md`.

pub mod matches;
pub mod notes;
pub mod nullifiers;
pub mod subscriptions;
pub mod tree_state;

use crate::domain::error::{AppError, AppResult};
use database::{DbConn, DbPool};

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`AppError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> AppResult<DbConn<'_>> {
    pool.get().await.map_err(|e| AppError::Db(e.to_string()))
}

/// Map a query failure to [`AppError::Db`].
///
/// Every statement in this layer ends in the same `map_err`: the driver string
/// names tables, columns and sometimes the failing value, so it must reach the
/// log rather than the response, which is what `AppError::Db` arranges.
pub(crate) fn db_err(e: diesel::result::Error) -> AppError {
    AppError::Db(e.to_string())
}
