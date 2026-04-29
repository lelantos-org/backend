use crate::domain::responses::{MatchOut, NoteOut, SubscriptionOut};
use crate::services::commitments::ChunkResponse;
use fmd_crypto::tree::MerkleTree;
use moka::future::Cache;
use shared::cache::build;
use std::sync::Arc;
use std::time::Duration;

pub struct TreeSnapshot {
    pub tree: MerkleTree,
}

pub type NotesPageKey = (Option<i64>, i64, i64);
pub type MatchesPageKey = (i64, i64, i64);
/// (chain_id, chunk_id) — only complete (immutable) chunks are stored.
pub type ChunkKey = (i64, u64);

#[derive(Clone)]
pub struct AppCache {
    pub tree: Cache<i64, Arc<TreeSnapshot>>,
    pub cm_to_leaf: Cache<(i64, Vec<u8>), i64>,
    pub subscriptions: Cache<(), Arc<Vec<SubscriptionOut>>>,
    pub notes_pages: Cache<NotesPageKey, Arc<Vec<NoteOut>>>,
    pub matches_pages: Cache<MatchesPageKey, Arc<Vec<MatchOut>>>,
    /// Positive-only spent-set cache. Spent bit is monotonic, so any
    /// `(chain_id, nf)` that has ever been observed `true` stays `true`.
    /// Negative results are NOT cached — they can flip to `true` any
    /// block.
    pub spent: Cache<(i64, Vec<u8>), ()>,
    /// Complete commitment chunks — immutable once full (1024 leaves), hence
    /// the long TTL. Capacity 64 keeps recently-completed chunks hot (highest
    /// server traffic, not yet CDN-cached); older ones fall back to the CDN's
    /// `max-age=31536000, immutable`.
    pub chunks: Cache<ChunkKey, Arc<ChunkResponse>>,
}

impl AppCache {
    pub fn new() -> Self {
        Self {
            tree: build(64, Duration::from_secs(5)),
            cm_to_leaf: build(100_000, Duration::from_secs(3600)),
            subscriptions: build(1, Duration::from_secs(30)),
            notes_pages: build(2_048, Duration::from_secs(3)),
            matches_pages: build(2_048, Duration::from_secs(3)),
            spent: build(100_000, Duration::from_secs(60)),
            chunks: build(64, Duration::from_secs(3_600)),
        }
    }
}

impl Default for AppCache {
    fn default() -> Self {
        Self::new()
    }
}
