use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct AnonymitySetQuery {
    /// Restrict to one chain. Absent means every chain.
    pub chain_id: Option<i64>,
    /// Restrict to one asset. Only meaningful together with `chainId`, since an
    /// asset id is unique within its chain.
    pub asset_id_u64: Option<i64>,
    /// Row cap, clamped by `dto::page_limit`. One row per distinct denomination,
    /// so a ladder-conforming pool needs far fewer than the default.
    pub limit: Option<i64>,
    /// Lookback for `recentCount`, in seconds. Does **not** filter `count`,
    /// which is always all-history.
    pub recent_sec: Option<i64>,
}
