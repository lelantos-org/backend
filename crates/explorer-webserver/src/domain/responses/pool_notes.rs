use serde::Serialize;
use utoipa::ToSchema;

/// One chain's commitment-tree occupancy.
///
/// Scale and liveness context for the anonymity-set figures, not a privacy score.
/// Never sum `leaves` across chains: each chain has its own tree, so notes on one
/// are no cover on another.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PoolNotesOut {
    pub chain_id: i64,
    /// Leaves committed to the tree, the contract's `committedCount`. Includes
    /// relayer fee notes and notes that have already been spent.
    pub leaves: i64,
    /// Of those leaves, how many are relayer fee notes: one per flushed deposit,
    /// since a deposit occupies two adjacent leaves. `leaves - feeNotes` is the
    /// count belonging to users.
    pub fee_notes: i64,
    /// Newest tree advance behind the count.
    pub last_ts: i64,
}
