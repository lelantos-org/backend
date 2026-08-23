use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::routing::{get, post};
use shared::request_span;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

pub fn build(state: AppState) -> Router {
    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(health))
        .route("/v1/quotes", post(handlers::post_quote))
        .layer(request_span::trace_layer())
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}
