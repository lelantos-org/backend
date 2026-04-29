use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetOut {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub token_hex: String,
    pub scale: String,
}
