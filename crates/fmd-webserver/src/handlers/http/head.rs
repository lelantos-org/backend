use crate::app::AppState;
use crate::domain::dto::HeadQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::HeadOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};

#[utoipa::path(
    get,
    path = "/v1/head",
    tag = "head",
    params(HeadQuery),
    responses((status = 200, body = HeadOut))
)]
#[tracing::instrument(skip(st), fields(chain_id = q.chain_id))]
pub async fn get_head(
    State(st): State<AppState>,
    Query(q): Query<HeadQuery>,
) -> AppResult<Json<HeadOut>> {
    Ok(Json(services::head::get(&st, q.chain_id).await?))
}
