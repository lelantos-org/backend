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
    /// The outcome of a submission another caller made under the same
    /// idempotency key. Carries only that caller's status and client message —
    /// see [`AppError::mirrored`].
    #[error("{message}")]
    Mirrored { status: StatusCode, message: String },
    /// A submission a contract guard rejected before it could be mined —
    /// caught by the `eth_call` pre-flight, so nothing was broadcast and no
    /// gas was spent.
    ///
    /// Distinct from [`AppError::Reverted`], which is a transaction that made
    /// it on-chain and failed there. The difference matters to the caller:
    /// this one is their payload to fix, and they cannot fix it without being
    /// told which guard refused. It surfaces as a gas-estimation failure, so
    /// without this variant it lands in [`AppError::Rpc`] — a 500 whose body
    /// is deliberately scrubbed, leaving "your adapter is not allowlisted"
    /// indistinguishable from "the relayer is broken".
    #[error("rejected by contract: {detail}")]
    ContractRejected {
        /// The contract's own revert text, safe to echo — see
        /// [`revert_reason`].
        reason: String,
        /// Full node error, for logs only. May embed the RPC URL.
        detail: String,
    },
}

/// The chain's revert reason, if `err` carries one.
///
/// Returns only the text from `execution reverted` onward. Everything before
/// that marker is node and transport detail — which is exactly where an RPC
/// URL, and with it an API key, would appear. What follows is the contract's
/// own message and its ABI-encoded data. A transport failure has no marker at
/// all and yields `None`, so it can never be mistaken for a revert.
pub fn revert_reason(err: &str) -> Option<String> {
    const MARKER: &str = "execution reverted";
    /// Long enough for a revert string plus its selector, short enough that a
    /// pathological node response cannot be used to flood a caller.
    const MAX: usize = 200;

    Some(err[err.find(MARKER)?..].trim().chars().take(MAX).collect())
}

impl AppError {
    /// Re-raise an error that another caller of the same idempotency key
    /// produced.
    ///
    /// Only what the client is told survives — the status and the message it
    /// would have been given. The original's internals stay with the caller
    /// that owns them, and were already logged there, so a shared failure
    /// cannot log the same node error twice or leak it to a second caller.
    pub fn mirrored(e: &AppError) -> Self {
        AppError::Mirrored {
            status: e.status(),
            message: e.client_message(),
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) | AppError::ContractRejected { .. } => StatusCode::BAD_REQUEST,
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
            AppError::Mirrored { status, .. } => *status,
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
            // Only `reason` — never `detail`, which is the raw node error.
            AppError::ContractRejected { reason, .. } => format!("rejected by contract: {reason}"),
            AppError::Reverted(_) => "submit reverted".into(),
            AppError::SubmitUnknown(_) => {
                "submit outcome unknown; check the chain before retrying".into()
            }
            AppError::MirrorDesynced(_) => "relayer unavailable for this chain".into(),
            // Already scrubbed by the caller that produced it.
            AppError::Mirrored { message, .. } => message.clone(),
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

    /// Mirroring hands one caller's failure to another, so it must scrub at
    /// least as hard as the original would have.
    #[test]
    fn a_mirrored_error_says_no_more_than_the_original() {
        for err in infrastructure_errors() {
            let mirrored = AppError::mirrored(&err);
            assert_eq!(mirrored.status(), err.status());
            assert_eq!(mirrored.client_message(), "internal error");
            assert!(!format!("{mirrored}").contains("example.com"));
        }

        // A caller's own error keeps the text that makes it actionable.
        let conflict = AppError::NullifierInFlight("chain 31337".into());
        let mirrored = AppError::mirrored(&conflict);
        assert_eq!(mirrored.status(), StatusCode::CONFLICT);
        assert_eq!(mirrored.client_message(), conflict.client_message());
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
