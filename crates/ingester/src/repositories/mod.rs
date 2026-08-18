pub mod atomic;
pub mod chain_state;
pub mod raw_events;

pub use atomic::{AtomicWriteRepo, PostgresAtomicWriteRepo};
pub use chain_state::{ChainStateRepo, PostgresChainStateRepo};
pub use raw_events::{CHANNEL_APPENDED, CHANNEL_REORG, PostgresRawEventRepo, RawEventRepo};

use crate::domain::error::IngesterError;
use database::DbPool;
use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;

pub(crate) type PooledConn<'a> =
    bb8::PooledConnection<'a, AsyncDieselConnectionManager<AsyncPgConnection>>;

/// Check a connection out of the pool.
///
/// Exists so repositories can say `checkout(&self.pool).await?` instead of
/// repeating a `map_err` over bb8's generic error type on every statement.
pub(crate) async fn checkout(pool: &DbPool) -> Result<PooledConn<'_>, IngesterError> {
    Ok(pool.get().await?)
}
