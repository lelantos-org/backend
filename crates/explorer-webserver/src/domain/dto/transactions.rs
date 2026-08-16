use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct RecentTxQuery {
    pub chain_id: Option<i64>,
    /// Only transactions at or after this unix second.
    pub since_ts: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct TxKindsQuery {
    pub chain_id: Option<i64>,
    pub bucket_sec: Option<i64>,
    pub since_ts: Option<i64>,
}
