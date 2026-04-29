use serde::Serialize;
use utoipa::ToSchema;

/// Per-bucket aggregated token flow. Amounts are token base units serialized
/// as decimal strings — they can exceed JS f64 safe range (uint256).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlowPoint {
    pub ts: i64,
    #[serde(rename = "in")]
    pub in_amount: String,
    #[serde(rename = "out")]
    pub out_amount: String,
}
