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

fn cc_layer(value: HeaderValue) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, value)
}

/// Screening is POST rather than a GET with a path parameter even though it is a
/// read: an address in the URL would reach access logs. The trace layer records
/// the path only (`shared::request_span::trace_layer`), and POST keeps address
/// safety from depending on that one layer's configuration. `no-store` on every
/// route keeps verdicts out of intermediary caches.
pub fn build(state: AppState) -> Router {
    let no_store = HeaderValue::from_static("no-store");

    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(handlers::health))
        .route("/v1/screen", post(handlers::screen))
        .route("/v1/screen/batch", post(handlers::screen_batch))
        .route("/v1/entries", get(handlers::list_entries))
        .layer(cc_layer(no_store))
        .layer(request_span::trace_layer())
        .with_state(state)
}
