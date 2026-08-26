use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeStateOut {
    pub chain_id: i64,
    pub leaf_count: i64,
    pub root_hex: String,
    /// `depth × 3` slots in the on-chain `filledSubtrees` layout, consumed by the
    /// relayer's tree_update witness builder.
    pub frontier_hex: Vec<Vec<String>>,
}
