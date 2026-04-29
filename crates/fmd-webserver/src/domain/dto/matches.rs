use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(rename_all = "camelCase")]
pub struct ListMatchesQuery {
    pub subscription: i64,
    pub after: Option<i64>,
    pub limit: Option<i64>,
}
