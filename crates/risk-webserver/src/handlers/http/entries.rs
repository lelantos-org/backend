use crate::app::AppState;
use crate::domain::dto::entries::ListEntriesQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::EntryOut;
use crate::repositories::screened_addresses::EntryFilter;
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
    let filter = EntryFilter {
        limit: q.clamped_limit(),
        offset: q.clamped_offset(),
        chain: q.chain,
        source: q.source,
    };
    Ok(Json(st.screening.list_entries(filter).await?))
}
