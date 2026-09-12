//! Transport- and service-level errors.
//!
//! An upstream failure's detail names the endpoint URL, which carries the API
//! key. Server-error bodies are therefore scrubbed; the tests below enforce it.
//!
//! Scope: this type covers failures of the transport or of this service, which
//! carry an HTTP status. A request refused by the allowlist is not an
//! `AppError` — it is a JSON-RPC error object returned with HTTP 200. See
//! [`crate::domain::jsonrpc`].

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    /// A body that is not a JSON-RPC request or batch at all.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// A chain id with no `[[chains]]` block.
    #[error("unsupported chain: {0}")]
    UnsupportedChain(u64),
    /// A batch larger than `max_batch`.
    #[error("batch too large: {got} entries, limit {limit}")]
    BatchTooLarge { got: usize, limit: usize },
    /// Over budget. Carries the whole seconds a client should wait, which
    /// becomes `Retry-After`.
    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },
    /// A request that weighs more than a whole rate-limit burst, and so can
    /// never be admitted however long the caller waits.
    ///
    /// HTTP 400, not 429 or 413: viem retries both of those, and a retry here
    /// fails identically every time. The message names the cost and the limit,
    /// which is what the caller needs to split the batch.
    #[error(
        "request costs {units} units, more than the {limit} this endpoint accepts at once; \
         send fewer calls per batch"
    )]
    TooCostly { units: u32, limit: u32 },
    /// The upstream never reached a verdict: a node that did not answer, a
    /// response that would not decode. Carries the driver string, which is
    /// logged but never returned.
    #[error("upstream: {0}")]
    Upstream(String),
    /// The upstream did not answer within the deadline.
    #[error("upstream timed out")]
    UpstreamTimeout,
    /// Every endpoint refused for being over *our* provider quota.
    ///
    /// HTTP 503 with `Retry-After`, not a 502: the service is not broken, it is
    /// out of credit for a moment, and the provider said for how long. Passing
    /// that on lets viem wait the provider's time instead of backing off blind.
    #[error("upstream rate limited")]
    UpstreamThrottled { retry_after_secs: u64 },
    /// A `Finalized`-class request with no archive endpoint reachable. Distinct
    /// from [`AppError::Upstream`] because it is a configuration or provisioning
    /// state rather than a transient failure, and it degrades a specific feature
    /// (the earned column) rather than the whole service.
    #[error("no archive upstream available")]
    NoArchiveUpstream,
    #[error("internal: {0}")]
    Internal(String),
}

/// Body for the two variants whose detail names the upstream endpoint.
const UPSTREAM_BODY: &str = "upstream rpc unavailable";

/// Body for [`AppError::Internal`], whose detail is this service's own wiring.
const INTERNAL_BODY: &str = "internal server error";

impl AppError {
    /// This variant's HTTP status and its log label.
    ///
    /// One match rather than two, so a new variant cannot be given a status here
    /// and omitted from `class`, or the reverse.
    fn kind(&self) -> (StatusCode, &'static str) {
        match self {
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::UnsupportedChain(_) => (StatusCode::NOT_FOUND, "unsupported_chain"),
            AppError::BatchTooLarge { .. } => (StatusCode::PAYLOAD_TOO_LARGE, "batch_too_large"),
            AppError::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            AppError::TooCostly { .. } => (StatusCode::BAD_REQUEST, "too_costly"),
            AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream"),
            AppError::UpstreamTimeout => (StatusCode::GATEWAY_TIMEOUT, "upstream_timeout"),
            AppError::UpstreamThrottled { .. } => {
                (StatusCode::SERVICE_UNAVAILABLE, "upstream_throttled")
            }
            AppError::NoArchiveUpstream => (StatusCode::BAD_GATEWAY, "no_archive_upstream"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }

    /// Stable, non-identifying label for logs and metrics.
    ///
    /// Log this rather than the error itself. `Upstream` carries a reqwest error
    /// whose text names the endpoint URL, and `Internal` carries this service's
    /// wiring detail. The variant name is enough to see which class is spiking.
    pub fn class(&self) -> &'static str {
        self.kind().1
    }

    pub fn status(&self) -> StatusCode {
        self.kind().0
    }

    /// What the caller is told.
    ///
    /// Errors describing the caller's own request echo their text; the ones
    /// carrying infrastructure detail do not. Without this split a 502 hands a
    /// browser the reqwest error, and that string contains the API key this
    /// whole service exists to keep server-side.
    pub fn client_message(&self) -> String {
        match self {
            AppError::BadRequest(_)
            | AppError::UnsupportedChain(_)
            | AppError::BatchTooLarge { .. }
            | AppError::RateLimited { .. }
            | AppError::TooCostly { .. }
            | AppError::UpstreamTimeout
            | AppError::UpstreamThrottled { .. }
            | AppError::NoArchiveUpstream => self.to_string(),
            AppError::Upstream(_) => UPSTREAM_BODY.into(),
            AppError::Internal(_) => INTERNAL_BODY.into(),
        }
    }

    /// One shared failure, restated for another caller waiting on the same
    /// upstream call — `AppError` is not `Clone`, and should not be.
    ///
    /// What a waiter can act on is kept: a timeout, a missing archive, how long
    /// to wait. Detail is dropped, since it names the upstream endpoint and one
    /// waiter's error must not widen what another is told; so is anything that
    /// described the leader's own request rather than the call they share.
    ///
    /// No wildcard arm, so a new variant has to decide here what a waiter sees
    /// instead of silently becoming a bare 502.
    pub fn restated(&self) -> AppError {
        match *self {
            AppError::UpstreamTimeout => AppError::UpstreamTimeout,
            AppError::NoArchiveUpstream => AppError::NoArchiveUpstream,
            AppError::RateLimited { retry_after_secs } => {
                AppError::RateLimited { retry_after_secs }
            }
            AppError::UpstreamThrottled { retry_after_secs } => {
                AppError::UpstreamThrottled { retry_after_secs }
            }
            AppError::BadRequest(_)
            | AppError::UnsupportedChain(_)
            | AppError::BatchTooLarge { .. }
            | AppError::TooCostly { .. }
            | AppError::Upstream(_)
            | AppError::Internal(_) => AppError::Upstream("upstream call failed".into()),
        }
    }
}

impl IntoResponse for AppError {
    /// `Retry-After` is set here rather than at the call site because it is the
    /// only thing that makes a 429 or an upstream-quota 503 actionable, and viem
    /// reads it: its `buildRequest` honours the header when retrying, so a
    /// browser recovers from a burst on its own with no SDK change.
    fn into_response(self) -> Response {
        let status = self.status();
        let body = self.client_message();
        // The one class nothing else logs: upstream failures are logged where
        // they happen, and the rest describe the caller's own request. The
        // detail is this service's wiring, withheld from the body, so the log
        // is the only place it is seen.
        if let AppError::Internal(detail) = &self {
            tracing::error!(class = self.class(), %detail, "internal error");
        }
        match self {
            AppError::RateLimited { retry_after_secs }
            | AppError::UpstreamThrottled { retry_after_secs } => (
                status,
                [("retry-after", retry_after_secs.max(1).to_string())],
                body,
            )
                .into_response(),
            _ => (status, body).into_response(),
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    const RPC_URL: &str = "https://arb-mainnet.g.alchemy.com/v2/SECRETAPIKEY";
    const CHAIN: u64 = 42161;

    /// Rejects a new `AppError` variant at compile time.
    ///
    /// `every_variant` is hand-written, so a new variant would otherwise go
    /// untested for the leak these cover. This wildcard-free match makes the
    /// compiler flag it; extend both.
    fn assert_listed(e: &AppError) {
        match e {
            AppError::BadRequest(_)
            | AppError::UnsupportedChain(_)
            | AppError::BatchTooLarge { .. }
            | AppError::RateLimited { .. }
            | AppError::TooCostly { .. }
            | AppError::Upstream(_)
            | AppError::UpstreamTimeout
            | AppError::UpstreamThrottled { .. }
            | AppError::NoArchiveUpstream
            | AppError::Internal(_) => {}
        }
    }

    /// Every variant, each carrying the payload it would carry in production.
    fn every_variant() -> Vec<AppError> {
        vec![
            AppError::BadRequest("not a json-rpc request".into()),
            AppError::UnsupportedChain(CHAIN),
            AppError::BatchTooLarge {
                got: 500,
                limit: 100,
            },
            AppError::RateLimited {
                retry_after_secs: 3,
            },
            AppError::TooCostly {
                units: 1500,
                limit: 180,
            },
            AppError::Upstream(format!("error sending request for url {RPC_URL}")),
            AppError::UpstreamTimeout,
            AppError::UpstreamThrottled {
                retry_after_secs: 7,
            },
            AppError::NoArchiveUpstream,
            AppError::Internal(format!("no upstream wired for {RPC_URL}")),
        ]
    }

    /// The load-bearing test for this whole service. The upstream URL carries
    /// the paid API key; a 5xx body that echoed the reqwest error would hand it
    /// to every browser that triggers an outage.
    #[test]
    fn no_server_error_body_echoes_the_upstream_url() {
        for e in every_variant() {
            assert_listed(&e);
            if !e.status().is_server_error() {
                continue;
            }
            let body = e.client_message();
            assert!(!body.contains(RPC_URL), "`{body}` echoes the endpoint URL");
            assert!(
                !body.contains("SECRETAPIKEY"),
                "`{body}` echoes the API key"
            );
        }
    }

    /// The class label reaches logs and metrics on every request, so it must
    /// carry no payload of its own.
    #[test]
    fn class_carries_no_payload() {
        for e in every_variant() {
            assert_listed(&e);
            let class = e.class();
            assert!(
                !class.contains(RPC_URL),
                "`{class}` echoes the endpoint URL"
            );
            assert!(
                !class.contains(&CHAIN.to_string()),
                "`{class}` echoes a chain id"
            );
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

    /// 4xx bodies still echo the caller's own input; only 5xx detail is
    /// scrubbed. Guards against `client_message` being applied too widely — a
    /// caller who sends a 500-entry batch has to be told the limit.
    #[test]
    fn a_client_error_body_is_unchanged() {
        let e = AppError::BatchTooLarge {
            got: 500,
            limit: 100,
        };
        assert_eq!(
            e.client_message(),
            "batch too large: 500 entries, limit 100"
        );
    }

    /// A `Retry-After` of zero tells a client to retry immediately, which is
    /// the one answer that makes a burst worse. Sub-second waits round up.
    #[test]
    fn retry_after_is_never_zero() {
        let res = AppError::RateLimited {
            retry_after_secs: 0,
        }
        .into_response();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res.headers().get("retry-after").unwrap(), "1");
    }

    /// A waiter on a shared call keeps what it can act on and loses the
    /// detail, which names the endpoint.
    #[test]
    fn a_restated_error_keeps_the_wait_and_drops_the_detail() {
        for e in every_variant() {
            let r = e.restated();
            assert!(!r.to_string().contains("SECRETAPIKEY"), "{r}");
            assert!(!r.client_message().contains("SECRETAPIKEY"), "{r}");
        }
        assert!(matches!(
            AppError::UpstreamThrottled {
                retry_after_secs: 7
            }
            .restated(),
            AppError::UpstreamThrottled {
                retry_after_secs: 7
            }
        ));
        assert!(matches!(
            AppError::RateLimited {
                retry_after_secs: 3
            }
            .restated(),
            AppError::RateLimited {
                retry_after_secs: 3
            }
        ));
    }

    /// Our provider quota running out is a 503 carrying the provider's wait,
    /// which viem honours, rather than a blind 502.
    #[test]
    fn an_upstream_quota_refusal_carries_retry_after() {
        let res = AppError::UpstreamThrottled {
            retry_after_secs: 7,
        }
        .into_response();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(res.headers().get("retry-after").unwrap(), "7");
    }

    /// A request that can never fit must be a status viem does not retry, and
    /// must not invite a retry with the header.
    #[test]
    fn a_request_that_can_never_fit_is_not_retryable() {
        let res = AppError::TooCostly {
            units: 1500,
            limit: 180,
        }
        .into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(res.headers().get("retry-after").is_none());
    }
}
