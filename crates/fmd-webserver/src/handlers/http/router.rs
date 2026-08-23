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
        .route(
            "/v1/notes",
            get(handlers::list_notes).layer(cc("public, max-age=3")),
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
        .with_state(state)
}
