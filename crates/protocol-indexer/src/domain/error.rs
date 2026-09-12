//! The crate's error type.
//!
//! Narrow on purpose: every failure here is a pool checkout, a query, an RPC
//! read or a malformed config, and the three `From` impls flatten `database`'s
//! own errors into [`ProtocolIndexerError::Db`].

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolIndexerError {
    #[error("config: {0}")]
    Config(String),
    #[error("db: {0}")]
    Db(String),
    #[error("query: {0}")]
    Query(#[from] diesel::result::Error),
    #[error("rpc: {0}")]
    Rpc(String),
}

impl From<database::reorg::ReorgError> for ProtocolIndexerError {
    fn from(e: database::reorg::ReorgError) -> Self {
        ProtocolIndexerError::Db(e.to_string())
    }
}

impl From<database::CursorError> for ProtocolIndexerError {
    fn from(e: database::CursorError) -> Self {
        ProtocolIndexerError::Db(e.to_string())
    }
}

impl From<database::RawEventsError> for ProtocolIndexerError {
    fn from(e: database::RawEventsError) -> Self {
        ProtocolIndexerError::Db(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ProtocolIndexerError>;
