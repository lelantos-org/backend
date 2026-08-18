use crate::domain::error::{FmdIndexerError, Result};
use database::{DbConn, DbPool};

pub mod cursor;
pub mod matches;
pub mod notes;
pub mod raw_events;
pub mod spent_nullifiers;
pub mod subscriptions;

/// Check a connection out of the pool.
///
/// Every repository method starts with this. Behind a helper so the
/// pool-error mapping is written once and the methods below open with the
/// query they actually run.
pub(crate) async fn conn(pool: &DbPool) -> Result<DbConn<'_>> {
    pool.get()
        .await
        .map_err(|e| FmdIndexerError::Db(e.to_string()))
}
