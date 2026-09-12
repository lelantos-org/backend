//! This crate's own HTTP error type.
//!
//! Deliberately not `shared::http::AppError`: the race needs variants the
//! shared type does not carry (`NoLiquidity`, `Timeout`, `AllVenuesFailed`)
//! and a severity ordering over them for picking one error out of a race in
//! which every venue failed. See
//! `backend/ARCHITECTURE.md`.

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
    /// A venue call that never reached a verdict: a node that did not answer, a
    /// response that would not decode, a quoter address that is not a quoter.
    /// Carries the driver string, which is logged but never returned.
    #[error("rpc: {0}")]
    Rpc(String),
    /// A quoter that did not answer within `race_deadline_ms`.
    #[error("quoter timed out")]
    Timeout,
    #[error("all venues failed")]
    AllVenuesFailed,
    #[error("internal: {0}")]
    Internal(String),
}

/// Body for [`AppError::Rpc`]. The driver string it carries names the RPC
/// endpoint, and an endpoint URL usually carries an API key.
const RPC_BODY: &str = "upstream quote source unavailable";

/// Body for [`AppError::Internal`], whose detail is this service's own wiring.
const INTERNAL_BODY: &str = "internal server error";

impl AppError {
    /// This variant's HTTP status and its log label.
    ///
    /// One match rather than two, so a new variant cannot be given a status
    /// here and omitted from `class`, or the reverse.
    fn kind(&self) -> (StatusCode, &'static str) {
        match self {
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::UnsupportedChain(_) => (StatusCode::NOT_FOUND, "unsupported_chain"),
            AppError::NoLiquidity => (StatusCode::UNPROCESSABLE_ENTITY, "no_liquidity"),
            AppError::Rpc(_) => (StatusCode::BAD_GATEWAY, "rpc"),
            AppError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "timeout"),
            AppError::AllVenuesFailed => (StatusCode::BAD_GATEWAY, "all_venues_failed"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }

    /// Stable, non-identifying label for logs.
    ///
    /// Log this instead of the error itself. The `Display` forms carry payload:
    /// `UnsupportedChain` a chain id, and `Rpc` and `Internal` a driver string
    /// echoing the failing call's arguments, which here is the token pair and
    /// amount that `post_quote` keeps out of its own fields. The variant name is
    /// enough to identify which class of failure is spiking.
    pub fn class(&self) -> &'static str {
        self.kind().1
    }

    pub fn status(&self) -> StatusCode {
        self.kind().0
    }

    /// What the caller is told.
    ///
    /// Errors that describe the caller's own request echo their text; the two
    /// that carry infrastructure detail do not. Without this split a 502 hands
    /// the caller the alloy transport error, and that string names the RPC
    /// endpoint the operator configured.
    pub fn client_message(&self) -> String {
        match self {
            AppError::BadRequest(_)
            | AppError::UnsupportedChain(_)
            | AppError::NoLiquidity
            | AppError::Timeout
            | AppError::AllVenuesFailed => self.to_string(),
            AppError::Rpc(_) => RPC_BODY.into(),
            AppError::Internal(_) => INTERNAL_BODY.into(),
        }
    }

    /// How strong a claim this error makes about the request, for choosing one
    /// error out of a race in which every venue failed. Higher wins.
    ///
    /// The ranking is about what was actually learned, not about severity to the
    /// operator. `NoLiquidity` is the weakest because it is the only variant
    /// that asserts something about the chain — that the pair has no pool — and
    /// reporting it over an `Rpc` or a `Timeout` would state that as fact on the
    /// strength of a venue that never got an answer.
    fn severity(&self) -> u8 {
        match self {
            // Our own wiring is broken; nothing downstream was even attempted.
            AppError::Internal(_) => 4,
            // A venue was asked and did not answer, or answered unintelligibly.
            AppError::Rpc(_) => 3,
            // Also no answer, and unlike `Rpc` likely to succeed on a retry.
            AppError::Timeout | AppError::AllVenuesFailed => 2,
            // The caller's own input, which at least one venue cannot serve.
            AppError::BadRequest(_) | AppError::UnsupportedChain(_) => 1,
            // A venue looked and found nothing. The weakest claim, and the only
            // one that is a statement about the pair rather than about us.
            AppError::NoLiquidity => 0,
        }
    }

    /// The single error to report when no quoter in a race produced a quote.
    ///
    /// Ties resolve to the first, so the answer follows the configured venue
    /// order and does not depend on which venue happened to finish first. An
    /// empty input cannot arise from the race — it only runs this when at least
    /// one quoter failed — and falls back to [`AppError::AllVenuesFailed`].
    pub fn worst(errors: impl IntoIterator<Item = AppError>) -> AppError {
        errors
            .into_iter()
            .min_by_key(|e| std::cmp::Reverse(e.severity()))
            .unwrap_or(AppError::AllVenuesFailed)
    }
}

impl IntoResponse for AppError {
    /// Deliberately does not log, unlike the relayer's equivalent. A quote names
    /// a pair and an amount, `post_quote` keeps both out of its own log fields,
    /// and an `error!(%self)` here would put whatever the driver echoed back
    /// into the same log line. The class is logged by `post_quote` instead, and
    /// the detail stays at `debug` in the adapter that produced it.
    fn into_response(self) -> Response {
        (self.status(), self.client_message()).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    const AMOUNT: &str = "1234567";
    const CHAIN: &str = "8453";
    const RPC_URL: &str = "https://eth.example.com/v2/SECRETAPIKEY";

    /// Rejects a new `AppError` variant at compile time.
    ///
    /// `every_variant` is a hand-written list, so a new variant would otherwise
    /// go untested for the leak these tests cover. This wildcard-free match makes
    /// the compiler flag it; extend both.
    fn assert_listed(e: &AppError) {
        match e {
            AppError::BadRequest(_)
            | AppError::UnsupportedChain(_)
            | AppError::NoLiquidity
            | AppError::Rpc(_)
            | AppError::Timeout
            | AppError::AllVenuesFailed
            | AppError::Internal(_) => {}
        }
    }

    /// Every variant, each carrying the payload it would carry in production: a
    /// token address, an amount, a chain id, an endpoint URL.
    fn every_variant() -> Vec<AppError> {
        vec![
            AppError::BadRequest(format!("bad token {TOKEN}")),
            AppError::UnsupportedChain(CHAIN.parse().unwrap()),
            AppError::NoLiquidity,
            AppError::Rpc(format!(
                "error sending request for url {RPC_URL}: tokenIn={TOKEN} amountIn={AMOUNT}"
            )),
            AppError::Timeout,
            AppError::AllVenuesFailed,
            AppError::Internal(format!("quoter panicked on {TOKEN} for {AMOUNT}")),
        ]
    }

    /// A token address or amount must never reach a log through the error path.
    /// `post_quote` scrubs the pair from its own fields, which is undone if the
    /// error logged alongside carries the same values in its `Display` form, as
    /// `Rpc` and `Internal` do when an `eth_call` failure echoes its arguments.
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

    /// 4xx bodies still echo the caller's own input; only the 5xx detail is
    /// scrubbed. Guards against `client_message` being applied too widely.
    #[test]
    fn the_response_body_is_unchanged() {
        let e = AppError::BadRequest("slippage 60000 exceeds 5000".into());
        assert_eq!(e.to_string(), "bad request: slippage 60000 exceeds 5000");
        assert_eq!(
            e.client_message(),
            "bad request: slippage 60000 exceeds 5000"
        );
    }

    /// The counterpart: no 5xx body may carry the driver string. `Rpc` is built
    /// from an alloy transport error, whose text names the endpoint URL, and an
    /// endpoint URL is normally an API key. Nothing about that belongs in a
    /// response to an unauthenticated caller.
    #[test]
    fn no_server_error_body_echoes_infrastructure_detail() {
        for e in every_variant() {
            assert_listed(&e);
            if !e.status().is_server_error() {
                continue;
            }
            let body = e.client_message();
            assert!(!body.contains(RPC_URL), "`{body}` echoes the endpoint URL");
            assert!(!body.contains(TOKEN), "`{body}` echoes a token address");
            assert!(!body.contains(AMOUNT), "`{body}` echoes an amount");
        }
    }

    /// A venue that never got an answer must outrank one that did: reporting
    /// `NoLiquidity` because the other quoter's node was down tells the caller
    /// the pair has no pool, which is a claim nothing supports.
    #[test]
    fn worst_prefers_an_unanswered_venue_over_a_verdict() {
        let e = AppError::worst([AppError::NoLiquidity, AppError::Rpc("boom".into())]);
        assert!(matches!(e, AppError::Rpc(_)), "{e}");

        let e = AppError::worst([AppError::NoLiquidity, AppError::Timeout]);
        assert!(matches!(e, AppError::Timeout), "{e}");
    }

    /// Every venue agreeing there is no pool is the one case that really is a
    /// 422, and it is what the SDK distinguishes from a transient failure.
    #[test]
    fn worst_of_unanimous_no_liquidity_is_no_liquidity() {
        let e = AppError::worst([AppError::NoLiquidity, AppError::NoLiquidity]);
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// A caller error survives the fold: an `amount_in` no venue can take must
    /// come back as a 400 the caller can fix, not as a 502 they will retry.
    #[test]
    fn worst_keeps_a_caller_error_over_no_liquidity() {
        let e = AppError::worst([
            AppError::NoLiquidity,
            AppError::BadRequest("amount_in exceeds uint128".into()),
        ]);
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    /// Ties follow configured venue order rather than completion order, so the
    /// same failure does not return two different messages run to run.
    #[test]
    fn worst_breaks_ties_on_the_first() {
        let e = AppError::worst([
            AppError::Rpc("first".into()),
            AppError::Rpc("second".into()),
        ]);
        assert_eq!(e.to_string(), "rpc: first");
    }
}
