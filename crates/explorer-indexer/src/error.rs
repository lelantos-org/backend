use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExplorerIndexerError {
    #[error("config: {0}")]
    Config(String),
    #[error("db: {0}")]
    Db(String),
    #[error("query: {0}")]
    Query(#[from] diesel::result::Error),
    #[error("rpc: {0}")]
    Rpc(String),
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

pub type Result<T> = std::result::Result<T, ExplorerIndexerError>;
