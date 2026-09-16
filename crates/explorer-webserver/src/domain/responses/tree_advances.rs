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

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CountPoint {
    pub ts: i64,
    pub count: i64,
}

/// 24-hour per-chain activity. `inflow`, `outflow` and `hourly_out` are reserved
/// for per-asset value tracking and are always 0. `tx_count` and `hourly_in`
/// carry aggregated `inserted` counts from `tree_advances`.
///
/// Every indexed chain appears, including those with no insertions in the
/// window: a `tx_count` of 0 means scanned and quiet, and only a chain absent
/// from the list is unindexed.
///
/// `hourly_in` is oldest-first over 24 whole hours: index 23 is the hour
/// containing the request, index 0 the hour 23 before it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainFlowOut {
    pub chain_id: i64,
    pub inflow: i64,
    pub outflow: i64,
    pub hourly_in: Vec<i64>,
    pub hourly_out: Vec<i64>,
    pub tx_count: i64,
}
