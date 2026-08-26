pub mod asset_flows;
pub mod assets;
pub mod cursor;
pub mod deposit_events;
pub mod raw_events;
pub mod tree_advances;

use crate::error::ExplorerIndexerError;
use database::{DbConn, DbPool};

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`ExplorerIndexerError::Db`].
pub(crate) async fn conn(pool: &DbPool) -> Result<DbConn<'_>, ExplorerIndexerError> {
    pool.get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))
}
