use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::http::{HeaderValue, header};
use axum::routing::{get, post};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

fn cc_layer(value: HeaderValue) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, value)
}

/// Screening is POST rather than GET-with-path-param even though it is a
/// read: `TraceLayer` records the request URI, so an address in the URL would
/// be copied into access logs. `no-store` on every route for the same reason —
/// verdicts must not sit in an intermediary cache.
pub fn build(state: AppState) -> Router {
    let no_store = HeaderValue::from_static("no-store");

    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(handlers::health))
        .route("/v1/screen", post(handlers::screen))
        .route("/v1/screen/batch", post(handlers::screen_batch))
        .route("/v1/entries", get(handlers::list_entries))
        .layer(cc_layer(no_store))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
