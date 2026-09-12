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

/// Config-derived and effectively static; a redeploy is what changes it.
const REGISTRY: &str = "public, max-age=60";
/// Never cacheable: a client polls this to decide whether the service is
/// reachable, and a cached answer would report a dead one as up.
const NO_STORE: &str = "no-store";
/// Spot prices, on their own policy: they go stale within the minute where a
/// registered asset does not. 60 s is long enough to collapse a herd of wallet
/// polls onto one upstream fetch and short enough that a moving market shows up
/// within the minute — the same trade the relayer's price route makes.
const PRICES: &str = "public, max-age=60";
/// The index history, written once every 30 minutes by the venue-APY worker.
const YIELD_INDEX: &str = "public, max-age=900";

pub fn build(state: AppState) -> Router {
    // The catalog moves only when the indexer registers an asset or repolls a
    // venue, and every caller gets the same body — so unlike the relayer's
    // routes these may sit in a shared cache. This service carries nothing
    // per-user, which is what makes that safe.
    let catalog = public_max_age(state.cfg.cache_ttl_s);

    let api = Router::new()
        .route(
            "/health",
            get(handlers::health).layer(cache_control(NO_STORE)),
        )
        .route(
            "/v1/chains",
            get(handlers::chains).layer(cache_control(REGISTRY)),
        )
        .route(
            "/v1/assets",
            get(handlers::list_assets).layer(cache_control_value(catalog)),
        )
        // Its own header rather than the catalog's: a price is stale within the
        // minute where a registered asset is not, and this body is identical for
        // every caller, so an edge cache in front of it serves every wallet.
        .route(
            "/v1/prices",
            get(handlers::prices).layer(cache_control(PRICES)),
        )
        // Longer than the catalog's: the sampler behind it writes every 30
        // minutes, so a shorter header only re-asks for rows that cannot have
        // changed. This body is the largest the service serves and is identical
        // for every wallet on a chain, so the edge cache in front of it is what
        // keeps that size off the origin.
        .route(
            "/v1/yield-index",
            get(handlers::yield_index).layer(cache_control(YIELD_INDEX)),
        );

    // The catalog moves on a redeploy or an indexer write, while every wallet
    // re-polls it on a timer — so most requests here are a client asking for a
    // body it already holds, which a validator answers with a 304. Applied to
    // the API routes only, not to the static Swagger bundle merged after it.
    let routes = api
        .layer(from_fn(etag))
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));

    // No route here takes a body.
    service_layers(routes, Limits::read_only()).with_state(state)
}
