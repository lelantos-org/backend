use crate::app::AppState;
use crate::domain::dto::PoolNotesQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::PoolNotesOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/pool-notes",
    tag = "pool-notes",
    params(PoolNotesQuery),
    responses((status = 200, body = [PoolNotesOut]))
)]
pub async fn pool_notes(
    State(st): State<AppState>,
    Query(q): Query<PoolNotesQuery>,
) -> AppResult<Json<Arc<Vec<PoolNotesOut>>>> {
    Ok(Json(
        services::pool_notes::per_chain(&st, q.chain_id).await?,
    ))
}
