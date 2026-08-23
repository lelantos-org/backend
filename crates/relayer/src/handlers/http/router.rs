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
        // Never cacheable: `useSystemHealth` in the webapp polls this to decide
        // whether to tell the user the relayer is reachable, and a cached
        // answer would report a dead relayer as up.
        .route("/health", get(handlers::health).layer(cc("no-store")))
        // Global deployment config — chain ids, RPCs, contract addresses, the
        // token table. Identical for every caller and carrying nothing
        // per-user, so it is the one route here a shared cache may hold.
        //
        // This header is load-bearing at the edge. `cache_chain_registry` in
        // infra/terraform/cache.tf caches this path with
        // `edge_ttl = respect_origin`, so without a Cache-Control to respect
        // Cloudflare would fall back to its own default TTL — cached, but for
        // an interval nothing here chose. 60s is enough to take the fetch off
        // the webapp's first-paint path and to collapse a herd onto the single
        // relayer, while keeping a redeploy visible within the minute.
        .route(
            "/chains",
            get(handlers::chains).layer(cc("public, max-age=60")),
        )
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
        .layer(request_span::trace_layer())
        .with_state(state)
}
