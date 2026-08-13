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
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("db: {0}")]
    Db(String),
    #[error("internal: {0}")]
    Internal(String),
}

/// Body returned for every 5xx, in place of the underlying error.
const INTERNAL_BODY: &str = "internal server error";

impl IntoResponse for AppError {
    /// 4xx bodies echo the message: it describes the caller's own input.
    /// 5xx bodies do not — `Db` carries the raw driver string, which leaks
    /// table, column and constraint names and sometimes the failing value.
    /// That detail goes to the log instead.
    fn into_response(self) -> Response {
        match &self {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()).into_response(),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()).into_response(),
            AppError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()).into_response(),
            AppError::Unauthorized(_) => {
                (StatusCode::UNAUTHORIZED, self.to_string()).into_response()
            }
            AppError::Db(detail) | AppError::Internal(detail) => {
                tracing::error!(error = %detail, "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_BODY).into_response()
            }
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;
