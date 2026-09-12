use crate::build_info;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthOut {
    pub version: &'static str,
    pub commit: &'static str,
}

#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses((status = 200, body = HealthOut))
)]
pub async fn health() -> Json<HealthOut> {
    Json(HealthOut {
        version: build_info::PKG_VERSION,
        commit: build_info::GIT_SHA,
    })
}
