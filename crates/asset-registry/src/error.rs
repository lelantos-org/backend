//! This crate's error type.
//!
//! Deliberately its own rather than `shared::http::AppError`: `relayer` is one of
//! the two binaries that define a crate-local error type, so a shared HTTP error
//! here would not be the one it maps to. Each consumer writes a single
//! `From<asset_registry::Error>` impl instead.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("db: {0}")]
    Db(String),
    /// A `NUMERIC` column that cannot be read as the integer it should be.
    /// Written by the indexer, so this is an internal fault rather than bad
    /// input — consumers map it to their internal-error variant.
    #[error("numeric: {0}")]
    Numeric(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Map into the shared HTTP error, for the webservers that use it.
///
/// Behind a feature so the crate does not drag axum into `relayer`, which
/// defines its own error type and maps this one itself.
///
/// `Numeric` means an indexer-written column would not parse — our fault on this
/// side of the boundary, never the caller's, so it is internal rather than 4xx.
#[cfg(feature = "http")]
impl From<Error> for shared::http::AppError {
    fn from(e: Error) -> Self {
        match e {
            Error::Db(m) => Self::Db(m),
            Error::Numeric(m) => Self::Internal(m),
        }
    }
}
