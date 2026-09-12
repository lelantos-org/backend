//! Conditional GET: a validator on the way out, a `304` on the way back in.

use axum::body::{Body, HttpBody, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Largest body this layer will buffer to hash.
///
/// A response above it is passed through untouched rather than held in memory
/// twice. Every JSON response these services produce is orders of magnitude
/// smaller; the bound exists so a future large or streamed body degrades to "no
/// `ETag`" instead of to a memory spike.
const MAX_ETAG_BODY: usize = 4 * 1024 * 1024;

/// Strong `ETag` on every buffered `200`, and `304 Not Modified` when the
/// client already holds that exact body.
///
/// Worth applying where a response is *large and repeatedly re-requested while
/// unchanged* — the analytic endpoints, whose figures move on a tick rather than
/// per request, and the catalogs a wallet re-polls on a timer. It is not worth
/// applying to a `no-store` route: a client that may not store the body has
/// nothing to revalidate, so the hash would be computed for no one.
///
/// The hash runs on the serialised body rather than on a version the handler
/// tracks, because there is no such version: these bodies are built from query
/// results, and hashing what actually goes on the wire cannot disagree with it.
///
/// Ordering: this must sit *outside* [`cache_control`] so the 304 it synthesises
/// carries the route's policy header, which a client needs to know how long the
/// revalidated response stays fresh.
pub async fn etag(req: Request, next: Next) -> Response {
    // Read off the request before `next` consumes it. Only a GET is a candidate,
    // so nothing is cloned for the requests that are not.
    let is_get = req.method() == Method::GET;
    let if_none_match = is_get
        .then(|| req.headers().get(header::IF_NONE_MATCH).cloned())
        .flatten();

    let response = next.run(req).await;

    // Only a cacheable success is a validator candidate, and a handler that set
    // its own `ETag` knows something about its body that this layer does not.
    if !is_get
        || response.status() != StatusCode::OK
        || response.headers().contains_key(header::ETAG)
    {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    // An exact size hint is what distinguishes a body already assembled in
    // memory from a stream this layer must not collect: collecting a stream
    // would hold the whole response and delay its first byte until its last.
    match body.size_hint().exact() {
        Some(len) if len <= MAX_ETAG_BODY as u64 => {}
        _ => return Response::from_parts(parts, body),
    }
    let Ok(bytes) = to_bytes(body, MAX_ETAG_BODY).await else {
        // Unreachable for the in-memory bodies the guard above admits, and the
        // body is consumed by the attempt, so there is nothing left to return
        // but an error.
        tracing::error!("etag: could not buffer an already-sized response body");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let tag = entity_tag(&bytes);
    parts.headers.insert(
        header::ETAG,
        HeaderValue::from_str(&tag).expect("a quoted hex digest is a valid header value"),
    );

    if if_none_match.is_some_and(|v| if_none_match_matches(&v, &tag)) {
        parts.status = StatusCode::NOT_MODIFIED;
        // The body is dropped, so the length of the one it stands in for must go
        // with it; axum re-derives an accurate `0` for the empty body. Without
        // the removal a client would be told to expect the full representation
        // it is not being sent. `Content-Type` stays: it still describes the
        // representation the client is holding.
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::empty());
    }

    Response::from_parts(parts, Body::from(bytes))
}

/// The quoted validator for one body.
///
/// SHA-256 truncated to 128 bits: an `ETag` is a cache validator, not a security
/// boundary, and 16 bytes keeps the header short while leaving an accidental
/// collision — which would serve a stale 304 — far below the odds of any other
/// failure in this path.
fn entity_tag(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    format!("\"{}\"", hex::encode(&Sha256::digest(bytes)[..16]))
}

/// Whether an `If-None-Match` header covers `tag`.
///
/// The header is a comma-separated list, or `*`. Comparison is the weak one RFC
/// 9110 mandates for this header, so a `W/`-prefixed candidate matches the same
/// tag — this layer only ever emits strong tags, but a client or intermediary
/// may weaken one on the way back.
fn if_none_match_matches(value: &HeaderValue, tag: &str) -> bool {
    let Ok(value) = value.to_str() else {
        return false;
    };
    value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == tag
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::cache_control;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// Dispatches one GET through the `etag` layer, optionally presenting an
    /// `If-None-Match`.
    async fn get_with(app: Router, inm: Option<&str>) -> Response {
        let mut req = Request::builder().uri("/x");
        if let Some(v) = inm {
            req = req.header(header::IF_NONE_MATCH, v);
        }
        app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
    }

    fn etag_app() -> Router {
        Router::new()
            .route(
                "/x",
                get(|| async { "hello" }).layer(cache_control("public, max-age=5")),
            )
            .layer(axum::middleware::from_fn(etag))
    }

    fn etag_of(res: &Response) -> String {
        res.headers()
            .get(header::ETAG)
            .expect("layer sets a validator")
            .to_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn test_a_buffered_body_gets_a_quoted_validator() {
        let res = get_with(etag_app(), None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let tag = etag_of(&res);
        assert!(tag.starts_with('"') && tag.ends_with('"'), "{tag}");
        // 16 bytes of digest as hex, plus the two quotes.
        assert_eq!(tag.len(), 34, "{tag}");
    }

    /// The point of the layer: the second request pays for headers only.
    #[tokio::test]
    async fn test_the_same_body_revalidates_to_304() {
        let tag = etag_of(&get_with(etag_app(), None).await);
        let res = get_with(etag_app(), Some(&tag)).await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        // Not the length of the body it stands in for: axum re-derives this one
        // from the empty body the layer substitutes.
        assert_eq!(
            res.headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok()),
            Some("0")
        );
    }

    /// A 304 that dropped the policy header would leave the client with a
    /// revalidated response and no idea how long it stays fresh.
    #[tokio::test]
    async fn test_a_304_keeps_the_route_cache_control() {
        let tag = etag_of(&get_with(etag_app(), None).await);
        let res = get_with(etag_app(), Some(&tag)).await;
        assert_eq!(
            res.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=5")
        );
    }

    #[tokio::test]
    async fn test_a_stale_validator_gets_the_body() {
        let res = get_with(etag_app(), Some("\"0123456789abcdef0123456789abcdef\"")).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// Both forms RFC 9110 requires this header to accept.
    #[tokio::test]
    async fn test_a_wildcard_and_a_weakened_tag_both_match() {
        let tag = etag_of(&get_with(etag_app(), None).await);
        assert_eq!(
            get_with(etag_app(), Some("*")).await.status(),
            StatusCode::NOT_MODIFIED
        );
        assert_eq!(
            get_with(etag_app(), Some(&format!("W/{tag}")))
                .await
                .status(),
            StatusCode::NOT_MODIFIED
        );
    }

    /// One entry of a list must match, not the list as a whole.
    #[tokio::test]
    async fn test_one_entry_of_a_list_matches() {
        let tag = etag_of(&get_with(etag_app(), None).await);
        let list = format!("\"deadbeefdeadbeefdeadbeefdeadbeef\", {tag}");
        assert_eq!(
            get_with(etag_app(), Some(&list)).await.status(),
            StatusCode::NOT_MODIFIED
        );
    }

    /// A different body must not revalidate against another body's validator.
    #[tokio::test]
    async fn test_a_changed_body_changes_the_validator() {
        let a = Router::new().route("/x", get(|| async { "one" }));
        let b = Router::new().route("/x", get(|| async { "two" }));
        let layer = || axum::middleware::from_fn(etag);
        let ta = etag_of(&get_with(a.layer(layer()), None).await);
        let tb = etag_of(&get_with(b.layer(layer()), None).await);
        assert_ne!(ta, tb);
    }

    /// Only a `200` is a validator candidate: a 404 body is an error message,
    /// and tagging it would let a client revalidate its way into caching one.
    #[tokio::test]
    async fn test_a_non_200_is_left_alone() {
        let app = Router::new()
            .route("/x", get(|| async { (StatusCode::NOT_FOUND, "gone") }))
            .layer(axum::middleware::from_fn(etag));
        let res = get_with(app, None).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(res.headers().get(header::ETAG), None);
    }

    /// A POST response is not a cache entry, so it gets no validator even when
    /// the layer sits over the whole router.
    #[tokio::test]
    async fn test_a_non_get_is_left_alone() {
        let app = Router::new()
            .route("/x", axum::routing::post(|| async { "ok" }))
            .layer(axum::middleware::from_fn(etag));
        let res = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/x")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get(header::ETAG), None);
    }
}
