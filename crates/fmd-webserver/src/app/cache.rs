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
/// hashing every note into a leaf is O(notes) Poseidon and dominates the cost of
/// serving tree state, while notes are append-only so all but the newest leaves
/// stay correct. The mutex serialises catch-up so a burst of requests performs
/// the append once.
pub struct TreeMirror {
    pub tree: MerkleTree,
}

pub type NotesPageKey = (Option<i64>, i64, i64);
/// Identity of one cached page of matches.
///
/// A named struct rather than a 4-tuple of `i64`: transposing two positions would
/// serve the wrong page, and `chain_id` must not be omitted, since one
/// subscription spans every chain it matched on and a key without it would hand
/// chain A's notes to a chain B caller.
///
/// `backfilled_through` is absent: it rides in the cached value, where brief
/// staleness re-delivers rows rather than skipping them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchesPageKey {
    pub subscription_id: i64,
    pub chain_id: i64,
    pub after: i64,
    pub limit: i64,
}
/// `(chain_id, chunk_id)`. Only complete, immutable chunks are stored.
pub type ChunkKey = (i64, u64);

#[derive(Clone)]
pub struct AppCache {
    /// No TTL: the mirror is advanced in place, never discarded.
    pub tree: Cache<i64, Arc<Mutex<TreeMirror>>>,
    pub notes_pages: Cache<NotesPageKey, Arc<Vec<NoteOut>>>,
    pub matches_pages: Cache<MatchesPageKey, Arc<MatchesPage>>,
    /// Complete commitment chunks, immutable once full at 1024 leaves, hence the
    /// long TTL. Capacity 64 keeps recently completed chunks hot, since they carry
    /// the highest origin traffic before the CDN caches them; older ones fall back
    /// to the CDN's `max-age=31536000, immutable`.
    pub chunks: Cache<ChunkKey, Arc<ChunkResponse>>,
    /// Complete spent-nullifier chunks, immutable like `chunks`: full at 1024
    /// entries, and `seq` grows only at the tail.
    pub nullifier_chunks: Cache<ChunkKey, Arc<NullifierChunkResponse>>,
    /// Total note count, used to bound γ at subscription time.
    ///
    /// `COUNT(*)` without a predicate is a full scan in Postgres, and the endpoint
    /// that needs it is unauthenticated and unthrottled, so uncaching it would
    /// mean one scan per request. The value only has to be accurate enough to pick
    /// a power of two, so a short TTL suffices.
    pub note_count: Cache<(), i64>,
}

impl AppCache {
    pub fn new() -> Self {
        Self {
            tree: build(64, Duration::from_secs(24 * 3_600)),
            // One second: `/v1/head` lets clients poll for movement at ~5s, so a
            // longer page TTL would consume much of the remaining budget. These
            // are per-caller keyed pages with modest hit rates, so the shorter
            // window costs little.
            notes_pages: build(2_048, Duration::from_secs(1)),
            matches_pages: build(2_048, Duration::from_secs(1)),
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
