//! What the two chunk feeds have in common: the page size, the row range one
//! chunk covers, and the cache path around the body each feed builds.
//!
//! Both feeds are fixed-size pages so a client can sync the whole set without
//! telling the server which entries it cares about; see the crate README.

use crate::app::cache::ChunkKey;
use crate::domain::error::AppResult;
use crate::domain::responses::RenderedChunk;
use shared::cache::Cache;
use std::future::Future;

/// Entries per chunk. Both feeds page on the same boundary, so a client walks
/// them with one counter.
pub const CHUNK_SIZE: u64 = 1024;

/// The half-open `[from, to)` ordinal range chunk `chunk_id` covers.
///
/// Saturating: `chunk_id` comes straight off the request path, and
/// `chunk_id * CHUNK_SIZE` overflows for values a caller can trivially send.
/// Clamping yields an empty, incomplete chunk for those rather than a panic or a
/// wrapped-around range.
pub fn range(chunk_id: u64) -> (i64, i64) {
    let from = chunk_id.saturating_mul(CHUNK_SIZE).min(i64::MAX as u64) as i64;
    (from, from.saturating_add(CHUNK_SIZE as i64))
}

/// Serve one chunk: the bytes rendered earlier if they are cached, otherwise
/// whatever `build` produces.
///
/// Only a *complete* chunk is stored. That rule lives here so the two feeds
/// cannot drift apart on it: the tail chunk grows with the next block, and
/// caching it would pin a short page for the cache's hour-long TTL.
///
/// `build` is a future rather than a rendered value because it must not run on a
/// hit; futures are lazy, so constructing it before the lookup costs nothing.
pub(crate) async fn serve<F>(
    cache: &Cache<ChunkKey, RenderedChunk>,
    metric: &'static str,
    key: ChunkKey,
    build: F,
) -> AppResult<RenderedChunk>
where
    F: Future<Output = AppResult<RenderedChunk>>,
{
    let cached = cache.get(&key).await;
    shared::metrics::record_cache(metric, cached.is_some());
    if let Some(cached) = cached {
        return Ok(cached);
    }

    let rendered = build.await?;
    if rendered.is_complete() {
        cache.insert(key, rendered.clone()).await;
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_covers_its_own_page() {
        assert_eq!(range(0), (0, 1024));
        assert_eq!(range(1), (1024, 2048));
        assert_eq!(range(7), (7168, 8192));
    }

    /// `chunk_id` is caller-supplied, so the multiply must not overflow.
    #[test]
    fn an_absurd_chunk_id_clamps_instead_of_wrapping() {
        let (from, to) = range(u64::MAX);
        assert!(from > 0 && to >= from, "{from}..{to}");
    }
}
