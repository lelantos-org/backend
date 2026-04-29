use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubscription {
    pub detection_key_hex: String,
    pub gamma: i32,
}
