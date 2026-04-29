use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(rename_all = "camelCase")]
pub struct PathQuery {
    pub chain_id: i64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(rename_all = "camelCase")]
pub struct TreeStateQuery {
    pub chain_id: i64,
}
