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
    #[error("join: {0}")]
    Join(#[from] tokio::task::JoinError),
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

/// Surface a unique violation that the statement's `ON CONFLICT` target did
/// not cover.
///
/// `notes` and `spent_nullifiers` each carry a second UNIQUE beyond the one the
/// insert names (`notes_chain_leaf_idx`, `spent_nullifiers_chain_seq_idx` and
/// `spent_nullifiers_chain_id_nf_key`). A collision there is not absorbed by
/// `DO NOTHING`: it aborts the statement, the tick fails, and the driver logs a
/// generic tick error while the cursor stops advancing. Naming the constraint
/// makes the cause visible.
pub fn log_unique_violation(table: &str, e: &diesel::result::Error) {
    use diesel::result::{DatabaseErrorKind, Error as DieselError};
    if let DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, info) = e {
        tracing::error!(
            table,
            constraint = info.constraint_name().unwrap_or("<unknown>"),
            detail = info.details().unwrap_or(""),
            "insert hit a unique constraint the ON CONFLICT target does not cover"
        );
    }
}
