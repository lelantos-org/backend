use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CountPoint {
    pub ts: i64,
    pub count: i64,
}
