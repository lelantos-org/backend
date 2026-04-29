use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, post};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

fn cc(value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static(value))
}

/// Per-method Cache-Control for `/v1/subscriptions`. GET serves from a 30s
/// moka cache so it can be reused by clients. POST mutates state.
async fn subscriptions_cc(req: Request, next: Next) -> Response {
    let is_get = req.method() == Method::GET;
    let mut resp = next.run(req).await;
    let value = if is_get {
        "private, max-age=30"
    } else {
        "no-store"
    };
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    resp
}

pub fn build(state: AppState) -> Router {
    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(handlers::health).layer(cc("no-store")))
        .route(
            "/v1/notes",
            get(handlers::list_notes).layer(cc("public, max-age=3")),
        )
        .route(
            "/v1/matches",
            get(handlers::list_matches).layer(cc("public, max-age=3")),
        )
        .route(
            "/v1/subscriptions",
            get(handlers::list_subscriptions)
                .post(handlers::create_subscription)
                .layer(middleware::from_fn(subscriptions_cc)),
        )
        .route(
            "/v1/subscriptions/:id",
            delete(handlers::delete_subscription).layer(cc("no-store")),
        )
        .route(
            "/v1/commitments/chunk/:chunk_id",
            get(handlers::get_commitment_chunk),
        )
        .route(
            "/v1/path/:cm",
            get(handlers::get_path).layer(cc("public, max-age=5")),
        )
        .route(
            "/v1/tree-state",
            get(handlers::get_tree_state).layer(cc("public, max-age=5")),
        )
        .route(
            "/v1/spent",
            post(handlers::check_spent).layer(cc("no-store")),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
