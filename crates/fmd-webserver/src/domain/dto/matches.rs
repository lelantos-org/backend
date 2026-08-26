use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(rename_all = "camelCase")]
pub struct ListMatchesQuery {
    /// Required. The feed spans every chain the subscription matched on, so a
    /// caller that omitted it would receive notes it cannot spend; the request is
    /// rejected rather than served across all chains.
    pub chain_id: i64,
    pub after: Option<i64>,
    pub limit: Option<i64>,
}
