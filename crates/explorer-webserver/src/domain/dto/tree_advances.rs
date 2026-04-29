use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ListTreeAdvancesQuery {
    pub chain_id: Option<i64>,
    /// Start-index strictly greater than this. Page through history by
    /// chaining the previous response's max start_index back into this field.
    pub since_start_index: Option<i64>,
    pub limit: Option<i64>,
}
