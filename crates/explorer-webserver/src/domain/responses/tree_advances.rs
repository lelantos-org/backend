use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TreeAdvanceOut {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub start_index: i64,
    pub inserted: i32,
    pub old_root_hex: String,
    pub new_root_hex: String,
    pub tx_hash_hex: String,
    pub block_ts: i64,
}
