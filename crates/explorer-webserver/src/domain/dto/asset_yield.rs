use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct YieldQuery {
    /// Restrict to one chain. Absent means every chain, as on `/v1/locked`.
    pub chain_id: Option<i64>,
}
