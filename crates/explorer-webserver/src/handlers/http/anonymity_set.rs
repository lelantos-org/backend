use crate::app::AppState;
use crate::domain::dto::{self, AnonymitySetQuery};
use crate::domain::error::AppResult;
use crate::domain::responses::AnonymitySetOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/anonymity-set",
    tag = "anonymity-set",
    params(AnonymitySetQuery),
    responses((status = 200, body = [AnonymitySetOut]))
)]
pub async fn anonymity_set(
    State(st): State<AppState>,
    Query(q): Query<AnonymitySetQuery>,
) -> AppResult<Json<Arc<Vec<AnonymitySetOut>>>> {
    let limit = dto::page_limit(q.limit);
    let recent_sec = dto::recent_sec(q.recent_sec);
    let now_ts = chrono::Utc::now().timestamp();
    Ok(Json(
        services::anonymity_set::denominations(
            &st,
            q.chain_id,
            q.asset_id_u64,
            limit,
            now_ts,
            recent_sec,
        )
        .await?,
    ))
}
