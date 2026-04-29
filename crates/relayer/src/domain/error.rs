use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unknown chain: {0}")]
    UnknownChain(i64),
    #[error("db: {0}")]
    Db(String),
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("prover: {0}")]
    Prover(String),
    #[error("submit reverted: {0}")]
    Reverted(String),
    #[error("nullifier already spent: {0}")]
    NullifierAlreadySpent(String),
    #[error("nullifier in flight: {0}")]
    NullifierInFlight(String),
    #[error("oracle: {0}")]
    Oracle(String),
    #[error("stale estimate: {0}")]
    StaleEstimate(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::UnknownChain(_) => StatusCode::NOT_FOUND,
            AppError::Reverted(_) => StatusCode::BAD_GATEWAY,
            AppError::StaleEstimate(_) => StatusCode::CONFLICT,
            AppError::NullifierAlreadySpent(_) | AppError::NullifierInFlight(_) => {
                StatusCode::CONFLICT
            }
            AppError::Db(_)
            | AppError::Rpc(_)
            | AppError::Prover(_)
            | AppError::Oracle(_)
            | AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
