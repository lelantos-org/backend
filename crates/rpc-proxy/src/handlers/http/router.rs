use crate::app::AppState;
use crate::handlers::http::rpc::post_rpc;
use axum::Router;
use axum::routing::{get, post};
use shared::router::{Limits, cache_control, service_layers};
use std::time::Duration;

/// Deadline for one request.
///
/// Sits between the upstream's 8s and the SDK transport's 15s, so a slow
/// upstream surfaces as this service's 503 rather than the client aborting
/// first — and there is room for one fallback attempt inside the client's own
/// budget.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);

/// Largest request body accepted.
///
/// This is what bounds the work a request can cause before the rate limiter has
/// seen it: the limiter charges by method and batch size, neither of which is
/// known until the body is parsed, so the body cap is the only thing standing
/// in front of that parse.
///
/// It binds before `max_batch` does in the worst case — 100 calls at the 8 KB
/// calldata cap would be ~800 KB — and that is deliberate. A real batch is
/// twenty calls of a few dozen calldata bytes, about 3 KB, so the margin here is
/// large for expected traffic.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// `no-store` throughout: a response body is a specific wallet's balance or a
/// specific deposit's log. This service keeps that correlation out of its own
/// logs; caching it in an intermediary would place it somewhere with no
/// equivalent policy.
pub fn build(state: AppState) -> Router {
    let routes = Router::new()
        .route("/health", get(health))
        .route("/v1/{chain_id}", post(post_rpc))
        .layer(cache_control("no-store"))
        .with_state(state);

    service_layers(
        routes,
        Limits {
            request_timeout: REQUEST_TIMEOUT,
            max_body_bytes: MAX_BODY_BYTES,
        },
    )
}

async fn health() -> &'static str {
    "ok"
}
