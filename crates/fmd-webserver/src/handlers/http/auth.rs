//! Capability token extraction.
//!
//! Tokens are carried in `Authorization` rather than the request URI. Proxies and
//! CDNs write the URI to access logs by default and browsers retain it in
//! history; neither applies to a request header.

use crate::domain::error::AppError;
use crate::domain::token::TokenHash;
use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;

/// A token presented as `Authorization: Bearer <hex>`, reduced to the form the
/// database stores. The raw credential does not outlive extraction.
pub struct CapabilityToken(TokenHash);

impl CapabilityToken {
    pub fn hash(&self) -> &TokenHash {
        &self.0
    }
}

/// An absent or non-bearer header is a malformed request and yields `401`. A
/// well-formed header naming an unregistered token yields `404` from the
/// repository lookup. The two stay distinct so the status never reveals whether a
/// given token exists.
fn malformed() -> AppError {
    AppError::Unauthorized("expected `Authorization: Bearer <token>`".to_string())
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for CapabilityToken {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(malformed)?;
        let (scheme, token) = raw.split_once(' ').ok_or_else(malformed)?;
        // RFC 7235 auth schemes are case-insensitive.
        if !scheme.eq_ignore_ascii_case("Bearer") {
            return Err(malformed());
        }
        Ok(Self(TokenHash::presented(token.trim())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn extract(header_value: Option<&str>) -> Result<TokenHash, AppError> {
        let mut builder = axum::http::Request::builder();
        if let Some(v) = header_value {
            builder = builder.header(header::AUTHORIZATION, v);
        }
        let (mut parts, _) = builder.body(()).unwrap().into_parts();
        CapabilityToken::from_request_parts(&mut parts, &())
            .await
            .map(|t| t.0)
    }

    #[tokio::test]
    async fn a_bearer_token_reaches_the_service_as_its_hash() {
        let expected = TokenHash::presented("abc123").unwrap();
        assert_eq!(extract(Some("Bearer abc123")).await.unwrap(), expected);
        // RFC 7235: the scheme is case-insensitive.
        assert_eq!(extract(Some("bearer abc123")).await.unwrap(), expected);
        assert_eq!(extract(Some("BEARER  abc123 ")).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn a_non_bearer_header_is_unauthorized() {
        for value in [None, Some(""), Some("abc123"), Some("Basic abc123")] {
            assert!(
                matches!(extract(value).await, Err(AppError::Unauthorized(_))),
                "accepted {value:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_token_is_not_distinguishable_from_an_unknown_one() {
        // `NotFound` rather than `Unauthorized`: the header is well-formed, so
        // the status must not reveal that the token cannot match a row.
        assert!(matches!(
            extract(Some("Bearer zz")).await,
            Err(AppError::NotFound(_))
        ));
    }
}
