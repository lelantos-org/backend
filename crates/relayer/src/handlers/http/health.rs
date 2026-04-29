use crate::app::version;
use crate::domain::responses::HealthResponse;
use axum::Json;

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        version: version::CARGO_PKG_VERSION,
        commit: version::GIT_SHA,
    })
}
