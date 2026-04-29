use crate::app::AppState;
use crate::domain::dto::ListMatchesQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::MatchOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/matches",
    tag = "matches",
    params(ListMatchesQuery),
    responses((status = 200, body = [MatchOut]))
)]
#[tracing::instrument(skip(st), fields(subscription = q.subscription))]
pub async fn list_matches(
    State(st): State<AppState>,
    Query(q): Query<ListMatchesQuery>,
) -> AppResult<Json<Arc<Vec<MatchOut>>>> {
    let after = q.after.unwrap_or(0);
    let limit = q.limit.unwrap_or(100).min(1000);
    Ok(Json(
        services::matches::list(&st, q.subscription, after, limit).await?,
    ))
}
