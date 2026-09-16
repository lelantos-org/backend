//! The in-process caches and the keys that address them.
//!
//! Cache *policy* lives here -- what is stored, under what key, for how long --
//! while the reads themselves go through `crate::services::cached`.

use crate::domain::responses::{MatchesPage, NoteOut, RenderedChunk};
use crate::domain::token::TokenHash;
use shared::cache::Cache;
use shared::cache::build;
use std::sync::Arc;
use std::time::Duration;

/// Identity of one cached page of notes.
///
/// A named struct for the same reason as [`MatchesPageKey`]: `after` and `limit`
/// are both `i64`, so a transposition would type-check and serve the wrong page.
///
/// `chain_id` is `Option` because `/v1/notes` may be asked for every chain at
/// once; `None` and a chain id are distinct pages and must not collapse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NotesPageKey {
    pub chain_id: Option<i64>,
    pub after: i64,
    pub limit: i64,
}

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
    pub notes_pages: Cache<NotesPageKey, Arc<Vec<NoteOut>>>,
    pub matches_pages: Cache<MatchesPageKey, Arc<MatchesPage>>,
    /// Complete commitment chunks, immutable once full at 1024 leaves, hence the
    /// long TTL. Capacity 64 keeps recently completed chunks hot, since they carry
    /// the highest origin traffic before the CDN caches them; older ones fall back
    /// to the CDN's `max-age=31536000, immutable`.
    ///
    /// Holds the *serialised* chunk rather than the struct: the body of a
    /// complete chunk is a constant, so a hit is a refcount bump on the bytes
    /// instead of a 1024-entry `serde` pass.
    pub chunks: Cache<ChunkKey, RenderedChunk>,
    /// Complete spent-nullifier chunks, immutable like `chunks`: full at 1024
    /// entries, and `seq` grows only at the tail. Serialised on the same terms.
    pub nullifier_chunks: Cache<ChunkKey, RenderedChunk>,
    /// `(subscription_id, backfilled_through_note_id)` behind a capability
    /// token.
    ///
    /// `/v1/matches` resolves its token on every request, so without this a page
    /// served entirely from `matches_pages` still cost a query. Same one-second
    /// TTL as the pages it gates, for the same reason: the watermark it carries
    /// rides in the cached page anyway, where a value that trails the row
    /// re-delivers rows rather than skipping them.
    ///
    /// A deleted subscription is evicted here by `services::subscriptions::delete`,
    /// which makes revocation immediate on the replica that served it. Another
    /// replica keeps its own copy for up to the TTL — acceptable because only
    /// the token holder can present the token, so the window exposes a caller's
    /// own data to itself and nothing to a third party.
    pub cursor_state: Cache<TokenHash, (i64, i64)>,
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
            // One second: `/v1/head` lets clients poll for movement at ~5s, so a
            // longer page TTL would consume much of the remaining budget. These
            // are per-caller keyed pages with modest hit rates, so the shorter
            // window costs little.
            notes_pages: build(2_048, Duration::from_secs(1)),
            matches_pages: build(2_048, Duration::from_secs(1)),
            cursor_state: build(2_048, Duration::from_secs(1)),
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
