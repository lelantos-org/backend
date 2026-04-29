use crate::app::AppState;
use crate::domain::dto::ListNotesQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::NoteOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/notes",
    tag = "notes",
    params(ListNotesQuery),
    responses((status = 200, body = [NoteOut]))
)]
#[tracing::instrument(skip(st), fields(chain_id = ?q.chain_id))]
pub async fn list_notes(
    State(st): State<AppState>,
    Query(q): Query<ListNotesQuery>,
) -> AppResult<Json<Arc<Vec<NoteOut>>>> {
    let after = q.after.unwrap_or(0);
    let limit = q.limit.unwrap_or(100).min(1000);
    Ok(Json(
        services::notes::list(&st, q.chain_id, after, limit).await?,
    ))
}
