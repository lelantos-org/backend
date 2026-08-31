use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeStateOut {
    pub chain_id: i64,
    pub leaf_count: i64,
    pub root_hex: String,
    /// `depth × 3` slots in the on-chain `filledSubtrees` layout.
    ///
    /// Not consumed by the relayer, which builds its witness frontier from its
    /// own mirror (`relayer::services::tree`). The readers are SDK clients, so
    /// this moves with `root_hex`: both are derived from the mirror below, and
    /// a mirror at the wrong depth gets both wrong together.
    pub frontier_hex: Vec<Vec<String>>,
}
