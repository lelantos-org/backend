//! Route table. Every route declares its own `Cache-Control`; there is no
//! router-wide default to inherit by omission.

use crate::app::AppState;
use crate::handlers::http as handlers;
use crate::handlers::http::openapi::ApiDoc;
use axum::Router;
use axum::middleware::from_fn;
use axum::routing::{get, post};
use shared::router::cache_control as cc;
use shared::router::{Limits, etag, service_layers};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

pub fn build(state: AppState) -> Router {
    let routes = Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(handlers::health).layer(cc("no-store")))
        // The gate in front of every expensive sync read. `no-store` because a
        // cached watermark is stale, and staleness is the latency this endpoint
        // exists to remove.
        .route("/v1/head", get(handlers::get_head).layer(cc("no-store")))
        // Cursor-paginated and re-polled: a wallet asks for the same page again
        // as it walks the feed, and a page it has already seen revalidates to a
        // 304 instead of resending every note in it.
        .route(
            "/v1/notes",
            get(handlers::list_notes)
                .layer(cc("public, max-age=1"))
                .layer(from_fn(etag)),
        )
        // Per-caller data keyed on a capability token: not cacheable by a shared
        // proxy, and not worth caching in the browser.
        .route(
            "/v1/matches",
            get(handlers::list_matches).layer(cc("no-store")),
        )
        .route(
            "/v1/subscriptions",
            post(handlers::create_subscription)
                .delete(handlers::delete_subscription)
                .layer(cc("no-store")),
        )
        // The two chunk feeds declare their policy per *response* rather than
        // per route, in `domain::responses::RenderedChunk`: a complete chunk is
        // `immutable` for a year and the growing tail chunk is `max-age=5`. A
        // `cc(..)` layer here is `overriding`, so it would flatten that
        // distinction and either pin the tail or re-fetch every completed chunk.
        .route(
            handlers::commitments::ROUTE,
            get(handlers::get_commitment_chunk),
        )
        .route(
            handlers::nullifiers::ROUTE,
            get(handlers::get_nullifier_chunk),
        )
        // Polled on a timer and unchanged between blocks, so most polls are a
        // revalidation rather than a fetch.
        .route(
            "/v1/tree-state",
            get(handlers::get_tree_state)
                .layer(cc("public, max-age=5"))
                .layer(from_fn(etag)),
        );

    // The default budget: the only body this service takes is a subscription — a
    // detection key, a γ and a 32-byte token — which the default cap leaves
    // ample room for.
    service_layers(routes, Limits::default()).with_state(state)
}
