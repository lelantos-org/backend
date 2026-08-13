use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;
use tracing::error;

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
    /// Broadcast succeeded but the outcome is unknown (no receipt within the
    /// timeout). The tree mirror must not be rolled back on this path.
    #[error("submit outcome unknown: {0}")]
    SubmitUnknown(String),
    #[error("tree mirror desynced: {0}")]
    MirrorDesynced(String),
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

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::UnknownChain(_) => StatusCode::NOT_FOUND,
            AppError::NullifierAlreadySpent(_)
            | AppError::NullifierInFlight(_)
            | AppError::StaleEstimate(_) => StatusCode::CONFLICT,
            AppError::Reverted(_) | AppError::SubmitUnknown(_) => StatusCode::BAD_GATEWAY,
            AppError::MirrorDesynced(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Db(_)
            | AppError::Rpc(_)
            | AppError::Prover(_)
            | AppError::Oracle(_)
            | AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// What the caller is told. Errors describing *their* request carry their
    /// own text; the rest get a fixed string, because infrastructure error
    /// text is not safe to echo — alloy RPC errors embed the node URL, which
    /// usually carries an API key. The full error is logged instead.
    pub fn client_message(&self) -> String {
        match self {
            AppError::BadRequest(_)
            | AppError::UnknownChain(_)
            | AppError::NullifierAlreadySpent(_)
            | AppError::NullifierInFlight(_)
            | AppError::StaleEstimate(_) => self.to_string(),
            AppError::Reverted(_) => "submit reverted".into(),
            AppError::SubmitUnknown(_) => {
                "submit outcome unknown; check the chain before retrying".into()
            }
            AppError::MirrorDesynced(_) => "relayer unavailable for this chain".into(),
            AppError::Db(_)
            | AppError::Rpc(_)
            | AppError::Prover(_)
            | AppError::Oracle(_)
            | AppError::Internal(_) => "internal error".into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            error!(error = %self, "request failed");
        }
        (status, self.client_message()).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The secret an RPC URL usually carries.
    const NODE_URL: &str = "https://mainnet.example.com/v3/deadbeefsecretkey";

    fn infrastructure_errors() -> Vec<AppError> {
        vec![
            AppError::Rpc(format!(
                "send_transaction: error sending request for {NODE_URL}"
            )),
            AppError::Db(format!("connection to {NODE_URL} failed")),
            AppError::Prover(format!("open zkey: /srv/keys/{NODE_URL}")),
            AppError::Oracle(format!("GET {NODE_URL}: status 500")),
            AppError::Internal(format!("signer key: {NODE_URL}")),
        ]
    }

    #[test]
    fn infrastructure_errors_never_reach_the_caller() {
        for err in infrastructure_errors() {
            let msg = err.client_message();
            assert_eq!(msg, "internal error", "leaked detail from {err:?}");
            assert!(!msg.contains("example.com"), "leaked host from {err:?}");
            assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    /// Whatever the revert or desync reason was, it is operator detail.
    #[test]
    fn chain_side_failures_are_summarised() {
        assert_eq!(
            AppError::Reverted(format!("tx 0xabc reverted at {NODE_URL}")).client_message(),
            "submit reverted"
        );
        assert!(
            !AppError::MirrorDesynced(NODE_URL.into())
                .client_message()
                .contains("example.com")
        );
    }

    /// Errors about the caller's own request must stay actionable.
    #[test]
    fn client_errors_keep_their_detail() {
        let err = AppError::BadRequest("transfer requires publicOut == 0".into());
        assert!(err.client_message().contains("publicOut"));
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);

        let err = AppError::UnknownChain(31337);
        assert!(err.client_message().contains("31337"));
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    /// A resubmit is a conflict, not a server fault — the client can act on it.
    #[test]
    fn nullifier_conflicts_are_client_errors() {
        for err in [
            AppError::NullifierInFlight("chain 1".into()),
            AppError::NullifierAlreadySpent("chain 1".into()),
        ] {
            assert_eq!(err.status(), StatusCode::CONFLICT);
        }
    }

    /// An ambiguous submit must not read as "safe to retry".
    #[test]
    fn an_unknown_outcome_tells_the_caller_to_check_the_chain() {
        let err = AppError::SubmitUnknown("no receipt".into());
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert!(err.client_message().contains("check the chain"));
    }
}
