use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionOut {
    pub id: i64,
    pub detection_key_hex: String,
    pub gamma: i32,
    pub active: bool,
}
