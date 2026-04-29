use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpentResponse {
    /// 0x-prefixed hex of the nullifiers from the request that are
    /// recorded as spent on chain. Subset, not parallel mask.
    pub spent: Vec<String>,
}
