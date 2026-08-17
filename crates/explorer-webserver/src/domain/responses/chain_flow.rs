use serde::Serialize;
use utoipa::ToSchema;

/// 24-hour per-chain activity. `inflow`/`outflow` and `hourly_out` are
/// reserved for future per-asset value tracking; today they are always 0.
/// `tx_count` and `hourly_in` reflect aggregated `inserted` counts from
/// `tree_advances`.
///
/// Every indexed chain appears, including those with no insertions in the
/// window: a `tx_count` of 0 means "scanned and quiet", and only a chain absent
/// from the list is one nobody indexes.
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
