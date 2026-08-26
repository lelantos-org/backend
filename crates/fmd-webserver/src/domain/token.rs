//! Subscription capability tokens.

use crate::domain::error::{AppError, AppResult};
use sha2::{Digest, Sha256};
use std::fmt;

/// Required token width. Clients derive their own tokens from the wallet's `ivk`
/// and the server cannot verify how one was generated, so width is the only
/// property it enforces.
const TOKEN_BYTES: usize = 32;

/// A capability token reduced to the form the database stores.
///
/// The token is a bearer credential, so `subscriptions` stores its SHA-256 rather
/// than the token itself and a disclosure of the table yields no usable
/// capability. Equality lookup is the only operation performed on it, and
/// constructing this type is the only way to address a subscription row, so a raw
/// token cannot reach the repository layer.
///
/// Unsalted and unstretched: the input is 32 uniform secret bytes rather than a
/// password, so there is no dictionary to precompute.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenHash(Vec<u8>);

/// Omits the digest, which is a stable per-subscriber identifier and must not
/// reach the log stream through a stray `{:?}`.
impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TokenHash").finish_non_exhaustive()
    }
}

impl TokenHash {
    /// For `/v1/matches` and `DELETE /v1/subscriptions`, where the caller
    /// presents a token it claims to hold.
    ///
    /// A malformed token and an unknown one both surface as `NotFound`, so
    /// neither endpoint becomes an existence oracle. Width is not checked, since a
    /// wrong-length token hashes to a value no row carries.
    pub fn presented(token_hex: &str) -> AppResult<Self> {
        let token =
            decode(token_hex).map_err(|_| AppError::NotFound("subscription".to_string()))?;
        Ok(Self::of(&token))
    }

    /// For `POST /v1/subscriptions`, where the caller supplies its own input.
    /// Failures are `BadRequest`, and width is enforced so a short token
    /// cannot be registered as a weak capability.
    pub fn registered(token_hex: &str) -> AppResult<Self> {
        let token = decode(token_hex)
            .map_err(|e| AppError::BadRequest(format!("invalid token hex: {e}")))?;
        if token.len() != TOKEN_BYTES {
            return Err(AppError::BadRequest(format!(
                "token must be {TOKEN_BYTES} bytes, got {}",
                token.len()
            )));
        }
        Ok(Self::of(&token))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn of(token: &[u8]) -> Self {
        Self(Sha256::digest(token).to_vec())
    }
}

fn decode(token_hex: &str) -> Result<Vec<u8>, hex::FromHexError> {
    hex::decode(token_hex.trim_start_matches("0x"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ab00000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn the_digest_is_what_leaves_this_type() {
        let hash = TokenHash::registered(TOKEN).unwrap();
        assert_eq!(hash.as_bytes().len(), 32);
        assert_ne!(hash.as_bytes(), hex::decode(TOKEN).unwrap());
        // Must be deterministic for a client to present its token twice.
        assert_eq!(hash, TokenHash::registered(TOKEN).unwrap());
    }

    #[test]
    fn both_constructors_agree_on_the_same_token() {
        // Registration and lookup must agree, or a client locks itself out on its
        // first request after registering.
        assert_eq!(
            TokenHash::registered(TOKEN).unwrap(),
            TokenHash::presented(TOKEN).unwrap()
        );
        assert_eq!(
            TokenHash::presented(TOKEN).unwrap(),
            TokenHash::presented(&format!("0x{TOKEN}")).unwrap()
        );
    }

    #[test]
    fn registration_enforces_the_width() {
        assert!(TokenHash::registered(&format!("0x{TOKEN}")).is_ok());
        assert!(matches!(
            TokenHash::registered("abcd"),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            TokenHash::registered("zz"),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn a_malformed_presented_token_is_not_an_oracle() {
        // `NotFound`, matching a well-formed token that no row carries.
        assert!(matches!(
            TokenHash::presented("zz"),
            Err(AppError::NotFound(_))
        ));
    }
}
