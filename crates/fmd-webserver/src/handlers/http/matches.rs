use crate::app::AppState;
use crate::domain::dto::{self as dto, ListMatchesQuery};
use crate::domain::error::AppResult;
use crate::domain::responses::MatchesPage;
use crate::handlers::http::auth::CapabilityToken;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/matches",
    tag = "matches",
    params(
        ListMatchesQuery,
        ("Authorization" = String, Header, description = "`Bearer <subscription capability token, 32-byte hex>`"),
    ),
    responses((status = 200, body = MatchesPage), (status = 401, description = "missing or malformed Authorization header"))
)]
// `q` carries only the paging cursor and is safe to record. The token arrives
// in `Authorization`, which no span here reads.
#[tracing::instrument(skip(st, token))]
pub async fn list_matches(
    State(st): State<AppState>,
    token: CapabilityToken,
    Query(q): Query<ListMatchesQuery>,
) -> AppResult<Json<Arc<MatchesPage>>> {
    let (subscription_id, backfilled_through) =
        services::subscriptions::cursor_state_for_token(&st, token.hash()).await?;
    let (after, limit) = dto::page(q.after, q.limit);
    Ok(Json(
        services::matches::list(&st, subscription_id, backfilled_through, after, limit).await?,
    ))
}
