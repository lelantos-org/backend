use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MerkleProofOut {
    pub leaf_index: i64,
    pub commitment_hex: String,
    /// Per-level siblings, `depth × 3` elements, each a 32-byte big-endian field
    /// element rendered as a hex string with `0x` prefix.
    pub path_elements_hex: Vec<Vec<String>>,
    pub path_indices: Vec<u8>,
    /// Root computed from the path. Caller MUST verify this is in
    /// `tree_advances.new_root` (or call the on-chain `isKnownRoot` view)
    /// before trusting it for spending.
    pub root_hex: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeStateOut {
    pub chain_id: i64,
    pub leaf_count: i64,
    pub root_hex: String,
    /// `depth × 3` slots in the layout the prior on-chain `filledSubtrees`
    /// storage exposed; consumed by the relayer's tree_update witness builder.
    pub frontier_hex: Vec<Vec<String>>,
}
