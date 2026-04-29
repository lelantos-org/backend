use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::http::{HeaderValue, header};
use axum::routing::get;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

fn cc_layer(value: HeaderValue) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, value)
}

pub fn build(state: AppState) -> Router {
    let analytic =
        HeaderValue::from_str(&format!("public, max-age={}", state.cfg.cache_ttl_s)).unwrap();
    let head = HeaderValue::from_static("public, max-age=5");
    let no_store = HeaderValue::from_static("no-store");

    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(handlers::health).layer(cc_layer(no_store)))
        .route(
            "/v1/assets",
            get(handlers::list_assets).layer(cc_layer(analytic.clone())),
        )
        .route(
            "/v1/tree-advances",
            get(handlers::list_tree_advances).layer(cc_layer(head)),
        )
        .route(
            "/v1/tx-counts",
            get(handlers::tx_counts).layer(cc_layer(analytic.clone())),
        )
        .route(
            "/v1/chain-flows-24h",
            get(handlers::chain_flows_24h).layer(cc_layer(analytic.clone())),
        )
        .route(
            "/v1/asset-flows",
            get(handlers::asset_flows).layer(cc_layer(analytic)),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
