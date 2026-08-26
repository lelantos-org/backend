pub mod matches;
pub mod notes;
pub mod nullifiers;
pub mod subscriptions;

use crate::domain::error::{AppError, AppResult};
use database::{DbConn, DbPool};

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`AppError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> AppResult<DbConn<'_>> {
    pool.get().await.map_err(|e| AppError::Db(e.to_string()))
}
