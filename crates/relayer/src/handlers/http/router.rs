use crate::app::AppState;
use crate::handlers::http as handlers;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

/// Largest submission body accepted. A transact payload is a few kB plus the
/// per-output ciphertexts; 256 kB is generous for that and far below the 2 MB
/// axum would otherwise buffer per request.
const MAX_BODY_BYTES: usize = 256 * 1024;

pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/chains", get(handlers::chains))
        .route("/v1/spend", post(handlers::submit_spend))
        .route("/v1/spend/estimate", post(handlers::estimate_spend))
        .route("/v1/swap", post(handlers::submit_swap))
        .route("/v1/swap/estimate", post(handlers::estimate_swap))
        .route("/v1/deposits/stream", get(handlers::deposits_stream))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
