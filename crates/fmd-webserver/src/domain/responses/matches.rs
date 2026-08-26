use serde::Serialize;
use utoipa::ToSchema;

/// A page of matches plus the subscription's backfill watermark.
///
/// The watermark exists because `matches` is filled from both ends at once: the
/// live filter tick inserts rows for notes at the head while `backfill_tick`
/// walks history upward from `backfilled_through_note_id`. A client advancing a
/// resume cursor to the highest `noteId` it had seen would step over the gap
/// backfill has not reached, and rows landing there later would never be
/// returned.
///
/// Clients must clamp their persisted cursor to this value. Rows above it are
/// still returned, so new notes do not wait for a backfill to finish; they are
/// re-delivered until the watermark passes them.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatchesPage {
    /// Every `noteId` at or below this has been scanned against this
    /// subscription's detection key. Monotonic.
    pub backfilled_through_note_id: i64,
    pub matches: Vec<MatchOut>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MatchOut {
    pub note_id: i64,
    pub chain_id: i64,
    pub block_number: i64,
    pub leaf_index: i64,
    pub commitment_hex: String,
    pub ciphertext_hex: String,
    /// Sender's ECDH ephemeral public point, packed. Same encoding as
    /// `NoteOut::eph_pub_packed_hex`.
    pub eph_pub_packed_hex: String,
}
