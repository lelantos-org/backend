use crate::app::AppState;
use crate::domain::dto::entries::ListEntriesQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::EntryOut;
use axum::Json;
use axum::extract::{Query, State};

#[utoipa::path(
    get,
    path = "/v1/entries",
    tag = "entries",
    params(ListEntriesQuery),
    responses((status = 200, body = [EntryOut]))
)]
pub async fn list_entries(
    State(st): State<AppState>,
    Query(q): Query<ListEntriesQuery>,
) -> AppResult<Json<Vec<EntryOut>>> {
    Ok(Json(st.screening.list_entries(q).await?))
}
