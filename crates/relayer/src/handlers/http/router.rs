use crate::app::AppState;
use crate::handlers::http as handlers;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::routing::{get, post};
use std::time::Duration;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Largest submission body accepted. A transact payload is a few kB plus the
/// per-output ciphertexts; 256 kB is generous for that and far below the 2 MB
/// axum would otherwise buffer per request.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Deadline for a request/response route.
///
/// A submission takes the chain's tree-mirror mutex, which is held from
/// reserve through confirmation, so a caller can be parked behind another
/// chain-mate's proof and receipt wait. Without a deadline it parks there
/// until it gives up on its own — and a stalled node used to mean it never
/// did. Sized above one submission's own worst case (two `receipt_timeout_s`
/// windows, 60 s each by default) plus a proof, so this trips on a genuinely
/// stuck server rather than on a busy one.
///
/// Answered 503 rather than the layer's default 408: the request was fine, the
/// relayer was not, and 503 is already what this service returns for
/// "unavailable, retry later" (see `AppError::MirrorDesynced`).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

pub fn build(state: AppState) -> Router {
    // `.layer()` wraps the routes declared *above* it, so a new route belongs
    // inside this block. One added after the merge below would silently escape
    // the deadline.
    let timed = Router::new()
        .route("/health", get(handlers::health))
        .route("/chains", get(handlers::chains))
        .route("/v1/spend", post(handlers::submit_spend))
        .route("/v1/spend/estimate", post(handlers::estimate_spend))
        .route("/v1/swap", post(handlers::submit_swap))
        .route("/v1/swap/estimate", post(handlers::estimate_swap))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ));

    // Held out of the deadline on purpose: an SSE response is long-lived by
    // design, and a timeout would cut every subscriber off on a fixed
    // interval.
    let streaming = Router::new().route("/v1/deposits/stream", get(handlers::deposits_stream));

    timed
        .merge(streaming)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
