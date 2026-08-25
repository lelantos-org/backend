use serde::Serialize;
use utoipa::ToSchema;

/// The two cursors a wallet syncs against, and nothing else.
///
/// Deliberately not a summary of chain state: this is polled far more often
/// than any other endpoint, so every field has to be an indexed `MAX()`. A
/// caller compares these to what it last saw and only pays for `/v1/notes`,
/// `/v1/matches` and the chunk feeds when one has moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeadOut {
    pub chain_id: i64,
    /// Highest `notes.id`, or 0 when the chain has none.
    pub max_note_id: i64,
    /// Highest `spent_nullifiers.seq`, or 0 when the chain has none.
    pub max_nullifier_seq: i64,
}
