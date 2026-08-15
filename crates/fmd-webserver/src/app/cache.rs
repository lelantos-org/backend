use crate::domain::responses::{MatchesPage, NoteOut};
use crate::services::commitments::ChunkResponse;
use crate::services::nullifiers::ChunkResponse as NullifierChunkResponse;
use fmd_crypto::tree::MerkleTree;
use moka::future::Cache;
use shared::cache::build;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Per-chain tree mirror.
///
/// Held behind a mutex and kept across requests rather than rebuilt on a TTL:
/// hashing every note into a leaf is O(notes) Poseidon and dominates the cost
/// of serving tree state, while notes are append-only, so all but the newest
/// leaves are already correct. The mutex serialises the catch-up so a burst
/// of requests does the append once.
pub struct TreeMirror {
    pub tree: MerkleTree,
}

pub type NotesPageKey = (Option<i64>, i64, i64);
pub type MatchesPageKey = (i64, i64, i64);
/// (chain_id, chunk_id) — only complete (immutable) chunks are stored.
pub type ChunkKey = (i64, u64);

#[derive(Clone)]
pub struct AppCache {
    /// No TTL: the mirror is advanced in place, never discarded.
    pub tree: Cache<i64, Arc<Mutex<TreeMirror>>>,
    pub notes_pages: Cache<NotesPageKey, Arc<Vec<NoteOut>>>,
    pub matches_pages: Cache<MatchesPageKey, Arc<MatchesPage>>,
    /// Complete commitment chunks — immutable once full (1024 leaves), hence
    /// the long TTL. Capacity 64 keeps recently-completed chunks hot (highest
    /// server traffic, not yet CDN-cached); older ones fall back to the CDN's
    /// `max-age=31536000, immutable`.
    pub chunks: Cache<ChunkKey, Arc<ChunkResponse>>,
    /// Complete spent-nullifier chunks. Same immutability argument as
    /// `chunks`: full at 1024 entries, and `seq` only ever grows at the tail.
    pub nullifier_chunks: Cache<ChunkKey, Arc<NullifierChunkResponse>>,
    /// Total note count, used to bound γ at subscription time.
    ///
    /// `COUNT(*)` without a predicate is a full scan in Postgres, and the
    /// endpoint that needs it is unauthenticated and unthrottled — uncached,
    /// it is a scan-per-request amplifier. The value only has to be accurate
    /// enough to pick a power of two, so a short TTL costs nothing.
    pub note_count: Cache<(), i64>,
}

impl AppCache {
    pub fn new() -> Self {
        Self {
            tree: build(64, Duration::from_secs(24 * 3_600)),
            notes_pages: build(2_048, Duration::from_secs(3)),
            matches_pages: build(2_048, Duration::from_secs(3)),
            chunks: build(64, Duration::from_secs(3_600)),
            nullifier_chunks: build(64, Duration::from_secs(3_600)),
            note_count: build(1, Duration::from_secs(30)),
        }
    }
}

impl Default for AppCache {
    fn default() -> Self {
        Self::new()
    }
}
