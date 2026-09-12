pub mod asset_flows;
pub mod cursor;
pub mod yield_fee_events;

use crate::domain::error::ExplorerIndexerError;
use database::{DbConn, DbPool};

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`ExplorerIndexerError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> Result<DbConn<'_>, ExplorerIndexerError> {
    pool.get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))
}
