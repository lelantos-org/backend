//! `Cache-Control` policy, applied one route at a time.

use axum::http::{HeaderValue, header};
use tower_http::set_header::SetResponseHeaderLayer;

/// `Cache-Control` for one route, replacing whatever the handler set.
///
/// Applied per route rather than once for the whole router: cacheability is a
/// property of the resource, not the service. A health probe and an all-time
/// aggregate sit in the same router and must not share a policy.
///
/// ```
/// use axum::{Router, routing::get};
///
/// let app: Router = Router::new()
///     .route("/health", get(|| async { "ok" }).layer(shared::router::cache_control("no-store")));
/// ```
pub fn cache_control(value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    cache_control_value(HeaderValue::from_static(value))
}

/// [`cache_control`] for a value not known at compile time, such as one built
/// from a configured TTL by [`public_max_age`].
pub fn cache_control_value(value: HeaderValue) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, value)
}

/// `public, max-age=<secs>`, for a TTL that comes from config.
///
/// `public` means a shared cache may hold the response, so this belongs only on
/// a route whose body is identical for every caller. Anything keyed on a token
/// or an address wants [`cache_control`] with `no-store` instead.
///
/// The `expect` cannot fire: `secs` renders as decimal digits, and the rest of
/// the string is a literal, so every byte is within the visible-ASCII range a
/// header value permits. Stated once here rather than at each call site, where
/// it was previously an `unwrap` in one router and a silent fallback to an
/// unrelated TTL in another.
pub fn public_max_age(secs: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("public, max-age={secs}"))
        .expect("decimal digits are a valid header value")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// Dispatches without binding a port; see `metrics`' tests for the same shape.
    async fn cache_control_of(app: Router, path: &str) -> Option<String> {
        let res = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        res.headers()
            .get(header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn test_the_layer_sets_the_header_on_its_own_route() {
        let app = Router::new().route(
            "/x",
            get(|| async { "ok" }).layer(cache_control("no-store")),
        );
        assert_eq!(
            cache_control_of(app, "/x").await.as_deref(),
            Some("no-store")
        );
    }

    /// The layer is `overriding`, not `if_not_present`: a route's declared policy
    /// must win over whatever a handler happened to set, or a handler could
    /// quietly make a private response shareable.
    #[tokio::test]
    async fn test_the_layer_overrides_a_header_the_handler_set() {
        let app = Router::new().route(
            "/x",
            get(|| async { ([(header::CACHE_CONTROL, "public, max-age=31536000")], "ok") })
                .layer(cache_control("no-store")),
        );
        assert_eq!(
            cache_control_of(app, "/x").await.as_deref(),
            Some("no-store")
        );
    }

    /// A per-route layer must not leak onto the routes beside it.
    #[tokio::test]
    async fn test_a_route_without_the_layer_is_left_alone() {
        let app = Router::new()
            .route(
                "/cached",
                get(|| async { "ok" }).layer(cache_control("no-store")),
            )
            .route("/plain", get(|| async { "ok" }));
        assert_eq!(cache_control_of(app, "/plain").await, None);
    }

    #[test]
    fn test_public_max_age_renders_the_configured_ttl() {
        assert_eq!(public_max_age(30), "public, max-age=30");
        assert_eq!(public_max_age(0), "public, max-age=0");
        assert_eq!(
            public_max_age(u64::MAX),
            format!("public, max-age={}", u64::MAX)
        );
    }
}
