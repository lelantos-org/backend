use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct LockedQuery {
    /// Restrict to one chain. Absent means every chain, the view the dashboard
    /// requests.
    pub chain_id: Option<i64>,
}
