use serde::Deserialize;
use utoipa::ToSchema;

/// Batch query of spent-nullifier state. `nullifiers` carries 0x-prefixed
/// hex (32 bytes each). Server returns the subset that is on-chain spent.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpentRequest {
    pub chain_id: i64,
    pub nullifiers: Vec<String>,
}
