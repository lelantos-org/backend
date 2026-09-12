//! Route table and the layers every route carries.

use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::routing::{get, post};
use shared::request_span;
use shared::router::cache_control;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// `no-store` on every route: a quote body names the pair and the amount a
/// caller is about to trade, which is the correlation `post_quote` goes out of
/// its way to keep out of this service's own logs. Storing it in an
/// intermediary cache instead would put it somewhere with no such policy.
pub fn build(state: AppState) -> Router {
    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(health))
        .route("/v1/quotes", post(handlers::post_quote))
        .layer(cache_control("no-store"))
        .layer(request_span::trace_layer())
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}
