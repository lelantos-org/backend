pub mod anonymity_set;
pub mod asset_flows;
pub mod asset_locked;
pub mod assets;
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
