use crate::app::AppState;
use crate::domain::dto::LockedQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::ChainLockedOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/locked",
    tag = "locked",
    params(LockedQuery),
    responses((status = 200, body = [ChainLockedOut]))
)]
pub async fn locked_by_chain(
    State(st): State<AppState>,
    Query(q): Query<LockedQuery>,
) -> AppResult<Json<Arc<Vec<ChainLockedOut>>>> {
    Ok(Json(services::locked::by_chain(&st, q.chain_id).await?))
}
