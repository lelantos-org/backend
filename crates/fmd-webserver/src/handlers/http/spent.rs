use crate::app::AppState;
use crate::domain::dto::SpentRequest;
use crate::domain::error::AppResult;
use crate::domain::responses::SpentResponse;
use crate::services;
use axum::Json;
use axum::extract::State;

#[utoipa::path(
    post,
    path = "/v1/spent",
    tag = "spent",
    request_body = SpentRequest,
    responses((status = 200, body = SpentResponse))
)]
#[tracing::instrument(skip(st, body), fields(chain_id = body.chain_id, n = body.nullifiers.len()))]
pub async fn check_spent(
    State(st): State<AppState>,
    Json(body): Json<SpentRequest>,
) -> AppResult<Json<SpentResponse>> {
    let spent = services::spent::resolve(&st, body.chain_id, body.nullifiers).await?;
    Ok(Json(SpentResponse { spent }))
}
