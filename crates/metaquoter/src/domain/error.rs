use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unsupported chain: {0}")]
    UnsupportedChain(u64),
    #[error("no liquidity for pair")]
    NoLiquidity,
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("all venues failed")]
    AllVenuesFailed,
    #[error("internal: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::UnsupportedChain(_) => StatusCode::NOT_FOUND,
            AppError::NoLiquidity => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::Rpc(_) | AppError::AllVenuesFailed => StatusCode::BAD_GATEWAY,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
