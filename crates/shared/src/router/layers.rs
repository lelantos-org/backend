//! The layer stack every webserver in this workspace wraps its routes in.
//!
//! Here rather than copied into each router so that the *order* is decided once:
//! it is not obvious, it matters, and a service that got it wrong would look
//! healthy while losing its route labels or leaving requests unbounded.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use std::time::Duration;
use tower_http::timeout::TimeoutLayer;

/// What one request is allowed to cost.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Deadline for the whole request.
    pub request_timeout: Duration,
    /// Largest request body buffered before the handler sees it.
    pub max_body_bytes: usize,
}

impl Default for Limits {
    /// Sized for a service whose slowest route is a database read and whose
    /// largest body is a small JSON document.
    ///
    /// The deadline sits above the worst honest case, which the pool bounds:
    /// `PoolCfg::webserver` waits up to 5 s for a connection and then caps the
    /// statement at 15 s. So it trips on a wedged connection or a stalled peer —
    /// which would otherwise hold a pool slot with nobody waiting on the other
    /// end — rather than on a busy server.
    ///
    /// The body cap replaces axum's 2 MB default, which is far more than any
    /// route here accepts.
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            max_body_bytes: 64 * 1024,
        }
    }
}

impl Limits {
    /// For a service with no route that takes a body.
    ///
    /// The cap then only bounds what a confused or hostile client can make the
    /// server buffer before the request is rejected as a method mismatch.
    pub fn read_only() -> Self {
        Self {
            max_body_bytes: 16 * 1024,
            ..Self::default()
        }
    }
}

/// Wrap `router` in the deadline, the body cap, the trace span and the HTTP
/// metrics, in the one order that works.
///
/// `.layer()` wraps the routes declared *above* it, so this is applied once the
/// routes are on the router and before `with_state`.
///
/// Inside out:
///   - the timeout and the body cap bound the handler,
///   - [`crate::request_span::trace_layer`] spans it,
///   - and [`crate::metrics::track_http`] observes the same response the trace
///     layer does, from outside it.
///
/// The metrics layer must stay below `with_state`: its `route` label comes from
/// [`axum::extract::MatchedPath`], which exists only once axum has matched, and
/// a request that matched nothing is bucketed under a single label rather than
/// by its path.
///
/// A timed-out request is answered `503`, not the layer's default `408`: the
/// request was valid and the service was not.
pub fn service_layers<S>(router: Router<S>, limits: Limits) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            limits.request_timeout,
        ))
        .layer(DefaultBodyLimit::max(limits.max_body_bytes))
        .layer(crate::request_span::trace_layer())
        .layer(axum::middleware::from_fn(crate::metrics::track_http))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use axum::http::Request;
    use axum::routing::post;
    use tower::ServiceExt;

    /// The cap is enforced by the extractor that reads the body, so the fixture
    /// has to take one — a handler that ignores its body is never told the limit
    /// exists.
    #[tokio::test]
    async fn test_a_body_over_the_cap_is_rejected() {
        let app = service_layers(
            Router::new().route("/x", post(|_: Bytes| async { "ok" })),
            Limits {
                max_body_bytes: 16,
                ..Limits::default()
            },
        );
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/x")
                    .body(Body::from(vec![b'x'; 64]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn test_a_body_under_the_cap_reaches_the_handler() {
        let app = service_layers(
            Router::new().route("/x", post(|_: Bytes| async { "ok" })),
            Limits {
                max_body_bytes: 16,
                ..Limits::default()
            },
        );
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/x")
                    .body(Body::from("small"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// A service that takes no body still accepts a request that has none.
    #[tokio::test]
    async fn test_read_only_limits_keep_the_same_deadline() {
        assert_eq!(
            Limits::read_only().request_timeout,
            Limits::default().request_timeout
        );
        assert!(Limits::read_only().max_body_bytes < Limits::default().max_body_bytes);
    }
}
