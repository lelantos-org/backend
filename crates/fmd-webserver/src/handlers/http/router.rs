use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::http::{HeaderValue, header};
use axum::routing::{get, post};
use shared::request_span;
use tower_http::set_header::SetResponseHeaderLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

fn cc(value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static(value))
}

pub fn build(state: AppState) -> Router {
    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(handlers::health).layer(cc("no-store")))
        // The gate in front of every expensive sync read. `no-store` because a
        // cached watermark is a stale one, and staleness here is precisely the
        // latency this endpoint exists to remove.
        .route("/v1/head", get(handlers::get_head).layer(cc("no-store")))
        .route(
            "/v1/notes",
            get(handlers::list_notes).layer(cc("public, max-age=1")),
        )
        // Per-caller data keyed on a capability token: never cacheable by a
        // shared proxy, and not worth caching in the browser either.
        .route(
            "/v1/matches",
            get(handlers::list_matches).layer(cc("no-store")),
        )
        .route(
            "/v1/subscriptions",
            post(handlers::create_subscription)
                .delete(handlers::delete_subscription)
                .layer(cc("no-store")),
        )
        .route(
            "/v1/chains/:chain_id/commitments/chunks/:chunk_id",
            get(handlers::get_commitment_chunk),
        )
        .route(
            "/v1/chains/:chain_id/nullifiers/chunks/:chunk_id",
            get(handlers::get_nullifier_chunk),
        )
        .route(
            "/v1/tree-state",
            get(handlers::get_tree_state).layer(cc("public, max-age=5")),
        )
        .layer(request_span::trace_layer())
        // Outside `trace_layer` so it observes the same responses the tracing
        // layer does. Must stay above `with_state` and below the routes: the
        // `route` label comes from `MatchedPath`, which only exists once axum
        // has matched, and a request that matched nothing is bucketed under a
        // single label rather than by its path.
        .layer(axum::middleware::from_fn(shared::metrics::track_http))
        .with_state(state)
}
