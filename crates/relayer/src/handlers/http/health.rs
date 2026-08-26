use crate::app::build_info;
use crate::domain::responses::HealthResponse;
use axum::Json;

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        version: build_info::PKG_VERSION,
        commit: build_info::GIT_SHA,
    })
}
