use crate::app::AppState;
use crate::handlers::http as handlers;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, StatusCode, header};
use axum::routing::{get, post};
use shared::request_span;
use std::time::Duration;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;

/// Cache-Control for one route, matching the helper of the same name in
/// fmd-webserver's router.
fn cc(value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static(value))
}

/// Largest submission body accepted. A transact payload is a few kB plus the
/// per-output ciphertexts, so 256 kB is ample and well below the 2 MB axum would
/// otherwise buffer per request.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Deadline for a request/response route.
///
/// A submission takes the chain's tree-mirror mutex, held from reserve through
/// confirmation, so a caller can wait behind another submission's proof and
/// receipt. Without a deadline a stalled node parks it indefinitely. Sized above
/// one submission's worst case, two `receipt_timeout_s` windows of 60 s each by
/// default plus a proof, so it trips on a stuck server rather than a busy one.
///
/// Answered 503 rather than the layer's default 408: the request was valid and
/// the relayer was not, and 503 is what this service already returns for
/// unavailable-retry-later (see `AppError::MirrorDesynced`).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

pub fn build(state: AppState) -> Router {
    // `.layer()` wraps the routes declared above it, so a new route belongs inside
    // this block; one added after the merge below escapes the deadline.
    let timed = Router::new()
        // Never cacheable: `useSystemHealth` in the webapp polls this to decide
        // whether the relayer is reachable, and a cached answer would report a
        // dead relayer as up.
        .route("/health", get(handlers::health).layer(cc("no-store")))
        // Global deployment config: chain ids, RPCs, contract addresses and the
        // token table. Identical for every caller and carrying nothing per-user,
        // so it is the one route here a shared cache may hold.
        //
        // The header matters at the edge. `cache_chain_registry` in
        // infra/terraform/cache.tf caches this path with
        // `edge_ttl = respect_origin`, so without a Cache-Control to respect,
        // Cloudflare would fall back to its own default TTL. 60s takes the fetch
        // off the webapp's first-paint path and collapses a request herd onto the
        // relayer while keeping a redeploy visible within the minute.
        .route(
            "/chains",
            get(handlers::chains).layer(cc("public, max-age=60")),
        )
        // Spot USD prices for the registered assets. Identical for every caller,
        // like `/chains`, but on its own route because it goes stale on its own
        // schedule; see `PricesResponse`. 60s matches the registry: long enough to
        // collapse a herd of wallet polls, short enough that a moving market shows
        // up within the minute.
        .route(
            "/v1/prices",
            get(handlers::prices).layer(cc("public, max-age=60")),
        )
        .route("/v1/deposit/estimate", post(handlers::estimate_deposit))
        .route("/v1/spend", post(handlers::submit_spend))
        .route("/v1/spend/estimate", post(handlers::estimate_spend))
        .route("/v1/swap", post(handlers::submit_swap))
        .route("/v1/swap/estimate", post(handlers::estimate_swap))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ));

    // Held outside the deadline: an SSE response is long-lived, and a timeout
    // would cut every subscriber off on a fixed interval.
    let streaming = Router::new().route("/v1/deposits/stream", get(handlers::deposits_stream));

    timed
        .merge(streaming)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(request_span::trace_layer())
        .with_state(state)
}
