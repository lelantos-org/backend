//! Shared HTTP plumbing for webserver crates.
//!
//! Owns the canonical `AppError` + `IntoResponse` mapping. Caches and
//! `AppState` stay per-crate because they reference crate-local response
//! types.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("db: {0}")]
    Db(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Db(_) | AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
