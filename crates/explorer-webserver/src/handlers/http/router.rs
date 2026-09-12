//! Route table.
//!
//! Every route declares its own `Cache-Control`; there is no router-wide default
//! to inherit by omission.

use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::middleware::from_fn;
use axum::routing::get;
use shared::router::{
    Limits, cache_control, cache_control_value, etag, public_max_age, service_layers,
};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// Head-tracking routes: the classified feed and the paginated tree-advance
/// list both follow the tip of the chain, so they take a fixed short TTL rather
/// than the configured analytic one.
const HEAD: &str = "public, max-age=5";
/// Never cacheable: a client polls this to decide whether the service is
/// reachable, and a cached answer would report a dead one as up.
const NO_STORE: &str = "no-store";

pub fn build(state: AppState) -> Router {
    let analytic = public_max_age(state.cfg.cache_ttl_s);
    let cc_analytic = || cache_control_value(analytic.clone());

    let api = Router::new()
        .route(
            "/health",
            get(handlers::health).layer(cache_control(NO_STORE)),
        )
        .route(
            "/v1/assets",
            get(handlers::list_assets).layer(cc_analytic()),
        )
        .route(
            "/v1/tree-advances",
            get(handlers::list_tree_advances).layer(cache_control(HEAD)),
        )
        .route(
            "/v1/tx-counts",
            get(handlers::tx_counts).layer(cc_analytic()),
        )
        .route(
            "/v1/chain-flows-24h",
            get(handlers::chain_flows_24h).layer(cc_analytic()),
        )
        .route(
            "/v1/locked",
            get(handlers::locked_by_chain).layer(cc_analytic()),
        )
        .route(
            "/v1/asset-flows",
            get(handlers::asset_flows).layer(cc_analytic()),
        )
        // The classified feed tracks the head of the chain, like tree-advances.
        .route(
            "/v1/transactions",
            get(handlers::recent_transactions).layer(cache_control(HEAD)),
        )
        .route("/v1/tx-kinds", get(handlers::tx_kinds).layer(cc_analytic()))
        .route(
            "/v1/anonymity-set",
            get(handlers::anonymity_set).layer(cc_analytic()),
        )
        .route(
            "/v1/pool-notes",
            get(handlers::pool_notes).layer(cc_analytic()),
        )
        .route(
            "/v1/yield",
            get(handlers::yield_assets).layer(cc_analytic()),
        );

    // Every route above answers from a cache whose entries turn over on a tick,
    // so a dashboard polling faster than the TTL asks repeatedly for a body it
    // already has; a validator turns those into 304s. Applied to the API routes
    // only — the Swagger bundle merged after it is static and large, and hashing
    // it per request would buy nothing.
    let routes = api
        .layer(from_fn(etag))
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));

    // No route here takes a body.
    service_layers(routes, Limits::read_only()).with_state(state)
}
