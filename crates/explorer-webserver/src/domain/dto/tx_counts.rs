use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct TxCountsQuery {
    pub chain_id: Option<i64>,
    pub bucket_sec: Option<i64>,
    pub since_ts: Option<i64>,
}
