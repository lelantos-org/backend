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

impl AppError {
    /// This variant's HTTP status and its log label.
    ///
    /// One match rather than two so a new variant cannot pick up a status here
    /// and be forgotten in `class`, or the reverse.
    fn kind(&self) -> (StatusCode, &'static str) {
        match self {
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::UnsupportedChain(_) => (StatusCode::NOT_FOUND, "unsupported_chain"),
            AppError::NoLiquidity => (StatusCode::UNPROCESSABLE_ENTITY, "no_liquidity"),
            AppError::Rpc(_) => (StatusCode::BAD_GATEWAY, "rpc"),
            AppError::AllVenuesFailed => (StatusCode::BAD_GATEWAY, "all_venues_failed"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }

    /// Stable, non-identifying label for logs.
    ///
    /// Log this instead of the error itself. The `Display` forms carry payload:
    /// `UnsupportedChain` a chain id, and `Rpc` and `Internal` a driver string
    /// that echoes the failing call's arguments — which for this service is the
    /// token pair and amount that `post_quote` deliberately keeps out of its own
    /// fields. A variant name is enough to tell an operator which class of
    /// failure is spiking.
    pub fn class(&self) -> &'static str {
        self.kind().1
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.kind().0, self.to_string()).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    const AMOUNT: &str = "1234567";
    const CHAIN: &str = "8453";

    /// Rejects a new `AppError` variant at compile time.
    ///
    /// `every_variant` is a hand-written list, so nothing about it alone forces
    /// an update when the enum grows — and a variant missing from it would
    /// silently go untested for the exact leak these tests exist to catch. This
    /// wildcard-free match is what makes the compiler say so; the fix is to
    /// extend both.
    fn assert_listed(e: &AppError) {
        match e {
            AppError::BadRequest(_)
            | AppError::UnsupportedChain(_)
            | AppError::NoLiquidity
            | AppError::Rpc(_)
            | AppError::AllVenuesFailed
            | AppError::Internal(_) => {}
        }
    }

    /// Every variant, each carrying the payload it would in production — a
    /// token address, an amount, a chain id.
    fn every_variant() -> Vec<AppError> {
        vec![
            AppError::BadRequest(format!("bad token {TOKEN}")),
            AppError::UnsupportedChain(CHAIN.parse().unwrap()),
            AppError::NoLiquidity,
            AppError::Rpc(format!(
                "eth_call reverted: tokenIn={TOKEN} amountIn={AMOUNT}"
            )),
            AppError::AllVenuesFailed,
            AppError::Internal(format!("quoter panicked on {TOKEN} for {AMOUNT}")),
        ]
    }

    /// A token address or amount must never reach a log through the error
    /// path. `post_quote` scrubs the pair from its own fields; that is undone
    /// if the error it logs alongside carries the same values in its `Display`
    /// form, which `Rpc` and `Internal` readily do — an `eth_call` failure
    /// echoes the call arguments.
    #[test]
    fn class_carries_no_payload() {
        for e in every_variant() {
            assert_listed(&e);
            let class = e.class();
            assert!(!class.contains(TOKEN), "`{class}` echoes a token address");
            assert!(!class.contains(AMOUNT), "`{class}` echoes an amount");
            assert!(!class.contains(CHAIN), "`{class}` echoes a chain id");
        }
    }

    /// One label per variant, so a spike in a class is attributable.
    #[test]
    fn class_is_distinct_per_variant() {
        let classes: Vec<_> = every_variant().iter().map(AppError::class).collect();
        let unique: std::collections::HashSet<_> = classes.iter().collect();
        assert_eq!(
            unique.len(),
            classes.len(),
            "two variants share a class label: {classes:?}"
        );
    }

    /// 4xx bodies still echo the caller's own input; only the *log* label is
    /// scrubbed. Guards against `kind` being mistaken for the response body.
    #[test]
    fn the_response_body_is_unchanged() {
        let e = AppError::BadRequest("slippage 60000 exceeds 5000".into());
        assert_eq!(e.to_string(), "bad request: slippage 60000 exceeds 5000");
    }
}
