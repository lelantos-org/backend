//! The two chunk feeds' wire types, and the pre-rendered body they are served
//! as.
//!
//! These live in `domain` rather than beside their handlers because the services
//! now build *and serialise* them: a complete chunk is immutable, so the bytes
//! are what belongs in the cache, and a handler that re-serialised a cached
//! struct would pay for a 1024-entry `serde` pass on every hit of a feed every
//! wallet downloads in full.

use axum::body::Bytes;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitmentEntry {
    pub leaf_index: i64,
    /// `Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)` as a `0x`-prefixed 32-byte
    /// field element: the Merkle leaf, ready to insert.
    pub leaf_hash: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitmentChunkOut {
    pub chunk_id: u64,
    pub entries: Vec<CommitmentEntry>,
    pub is_complete: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NullifierChunkOut {
    pub chunk_id: u64,
    /// 0x-prefixed hex, ascending by insertion order. Each entry is the low 10
    /// bytes of the nullifier rather than all 32, since the client only tests set
    /// membership. `WIRE_BYTES` in `services::nullifiers` documents the width and
    /// its collision bound.
    pub nullifiers: Vec<String>,
    /// `false` marks the tail chunk, where the client stops paging.
    pub is_complete: bool,
}

/// A chunk body, whatever the feed.
///
/// The completeness flag is part of the wire type, and it also decides both the
/// freshness policy and whether the chunk may be cached at all — so it is read
/// back off the body rather than passed alongside it, where a caller could pass
/// one that disagrees.
pub trait ChunkBody: Serialize {
    fn is_complete(&self) -> bool;
}

impl ChunkBody for CommitmentChunkOut {
    fn is_complete(&self) -> bool {
        self.is_complete
    }
}

impl ChunkBody for NullifierChunkOut {
    fn is_complete(&self) -> bool {
        self.is_complete
    }
}

/// A chunk response serialised once and served many times.
///
/// Cloning is a refcount bump on the [`Bytes`], which is the whole point: a
/// cache hit costs a header write and a clone rather than a rebuild of the
/// 1024-entry body.
#[derive(Debug, Clone)]
pub struct RenderedChunk {
    body: Bytes,
    /// Copied off the body at render time; see [`ChunkBody`].
    is_complete: bool,
}

/// A chunk that will never change again: full at 1024 entries, and both feeds
/// only ever append.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// The tail chunk, which the next block extends.
const TAIL: &str = "public, max-age=5";

impl RenderedChunk {
    /// Serialise one chunk body.
    ///
    /// Fails only if `body` is not representable as JSON, which for these two
    /// plain structs cannot happen; the error is returned rather than unwrapped
    /// so the type stays usable for any future body.
    pub fn render(body: &impl ChunkBody) -> serde_json::Result<Self> {
        Ok(Self {
            is_complete: body.is_complete(),
            body: Bytes::from(serde_json::to_vec(body)?),
        })
    }

    /// Serialised length, for the feed-volume counter.
    pub fn byte_len(&self) -> usize {
        self.body.len()
    }

    /// Whether this chunk is full, and so will never change again. Decides both
    /// the freshness policy below and whether `services::chunks` may cache it.
    pub fn is_complete(&self) -> bool {
        self.is_complete
    }
}

impl IntoResponse for RenderedChunk {
    /// Sets the freshness policy here rather than in the handlers, so the two
    /// feeds cannot drift apart on the one header that decides whether a
    /// completed chunk is ever re-fetched.
    fn into_response(self) -> Response {
        let cache = if self.is_complete { IMMUTABLE } else { TAIL };
        (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            ],
            self.body,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::HttpBody;

    fn chunk(is_complete: bool) -> RenderedChunk {
        RenderedChunk::render(&NullifierChunkOut {
            chunk_id: 3,
            nullifiers: vec!["0x00000000000000000001".into()],
            is_complete,
        })
        .expect("a plain struct serialises")
    }

    /// The wire shape is what every wallet parses; rendering must not change it.
    #[test]
    fn test_the_body_is_the_camel_case_json_the_handler_used_to_produce() {
        let rendered = chunk(true);
        assert_eq!(
            String::from_utf8(rendered.body.to_vec()).unwrap(),
            r#"{"chunkId":3,"nullifiers":["0x00000000000000000001"],"isComplete":true}"#
        );
    }

    #[test]
    fn test_a_complete_chunk_is_served_as_immutable() {
        let res = chunk(true).into_response();
        assert_eq!(
            res.headers().get(header::CACHE_CONTROL).unwrap(),
            &HeaderValue::from_static(IMMUTABLE)
        );
    }

    /// The tail chunk grows, so it must not be pinned for a year.
    #[test]
    fn test_the_tail_chunk_is_served_with_a_short_ttl() {
        let res = chunk(false).into_response();
        assert_eq!(
            res.headers().get(header::CACHE_CONTROL).unwrap(),
            &HeaderValue::from_static(TAIL)
        );
    }

    /// `record_chunk_feed_bytes` reads the body's exact size hint, so a
    /// pre-rendered body must still report one.
    #[test]
    fn test_the_response_body_reports_its_exact_length() {
        let rendered = chunk(true);
        let len = rendered.byte_len();
        let res = rendered.into_response();
        assert_eq!(res.body().size_hint().exact(), Some(len as u64));
    }
}
