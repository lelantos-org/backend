//! The crate's error type.
//!
//! Deliberately narrow: this binary is database-in, database-out, so every
//! failure is either a pool checkout or a query. There is no RPC variant
//! because there is no RPC — see the crate README.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ExplorerIndexerError>;

#[derive(Debug, Error)]
pub enum ExplorerIndexerError {
    /// A pool checkout, or a `database` error flattened to its message.
    #[error("db: {0}")]
    Db(String),
    #[error("query: {0}")]
    Query(#[from] diesel::result::Error),
}

impl From<database::reorg::ReorgError> for ExplorerIndexerError {
    fn from(e: database::reorg::ReorgError) -> Self {
        ExplorerIndexerError::Db(e.to_string())
    }
}

impl From<database::CursorError> for ExplorerIndexerError {
    fn from(e: database::CursorError) -> Self {
        ExplorerIndexerError::Db(e.to_string())
    }
}

impl From<database::RawEventsError> for ExplorerIndexerError {
    fn from(e: database::RawEventsError) -> Self {
        ExplorerIndexerError::Db(e.to_string())
    }
}
