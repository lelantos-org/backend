use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::http::{HeaderValue, header};
use axum::routing::get;
use shared::request_span;
use tower_http::set_header::SetResponseHeaderLayer;
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
            get(handlers::list_tree_advances).layer(cc_layer(head.clone())),
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
            "/v1/locked",
            get(handlers::locked_by_chain).layer(cc_layer(analytic.clone())),
        )
        .route(
            "/v1/asset-flows",
            get(handlers::asset_flows).layer(cc_layer(analytic.clone())),
        )
        // The classified feed tracks the head of the chain, like tree-advances.
        .route(
            "/v1/transactions",
            get(handlers::recent_transactions).layer(cc_layer(head)),
        )
        .route(
            "/v1/tx-kinds",
            get(handlers::tx_kinds).layer(cc_layer(analytic.clone())),
        )
        .route(
            "/v1/anonymity-set",
            get(handlers::anonymity_set).layer(cc_layer(analytic.clone())),
        )
        .route(
            "/v1/pool-notes",
            get(handlers::pool_notes).layer(cc_layer(analytic.clone())),
        )
        .route(
            "/v1/yield",
            get(handlers::yield_assets).layer(cc_layer(analytic)),
        )
        .layer(request_span::trace_layer())
        .with_state(state)
}
