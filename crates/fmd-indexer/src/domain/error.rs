use thiserror::Error;

pub type Result<T> = std::result::Result<T, FmdIndexerError>;

#[derive(Debug, Error)]
pub enum FmdIndexerError {
    #[error("config: {0}")]
    Config(String),
    #[error("db: {0}")]
    Db(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("crypto: {0}")]
    Crypto(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
}

impl From<diesel::result::Error> for FmdIndexerError {
    fn from(e: diesel::result::Error) -> Self {
        FmdIndexerError::Db(e.to_string())
    }
}

impl From<database::reorg::ReorgError> for FmdIndexerError {
    fn from(e: database::reorg::ReorgError) -> Self {
        FmdIndexerError::Db(e.to_string())
    }
}

impl From<database::CursorError> for FmdIndexerError {
    fn from(e: database::CursorError) -> Self {
        FmdIndexerError::Db(e.to_string())
    }
}
