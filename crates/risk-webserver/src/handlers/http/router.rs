use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::routing::{get, post};
use shared::router::{Limits, cache_control, service_layers};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// Screening is POST rather than a GET with a path parameter even though it is a
/// read: an address in the URL would reach access logs. The trace layer records
/// the path only (`shared::request_span::trace_layer`), and POST keeps address
/// safety from depending on that one layer's configuration. `no-store` on every
/// route keeps verdicts out of intermediary caches.
///
/// No conditional-GET layer here, unlike the other webservers, for the same
/// reason: a client that may not store a verdict has nothing to revalidate.
pub fn build(state: AppState) -> Router {
    let routes = Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(handlers::health))
        .route("/v1/screen", post(handlers::screen))
        .route("/v1/screen/batch", post(handlers::screen_batch))
        .route("/v1/entries", get(handlers::list_entries))
        .layer(cache_control("no-store"));

    // The default budget: the largest legitimate body is a `MAX_BATCH` batch of
    // 100 addresses, a few kB, which the default cap leaves ample room for.
    service_layers(routes, Limits::default()).with_state(state)
}
