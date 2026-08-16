use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(rename_all = "camelCase")]
pub struct ListMatchesQuery {
    /// Required: the feed spans every chain the subscription matched on, and
    /// a caller that omitted it would receive notes it cannot spend. Rejecting
    /// the request is the safe failure; silently serving all chains is not.
    pub chain_id: i64,
    pub after: Option<i64>,
    pub limit: Option<i64>,
}
