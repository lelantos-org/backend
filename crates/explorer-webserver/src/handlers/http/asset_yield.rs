use crate::app::AppState;
use crate::domain::dto::YieldQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::YieldAssetOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/yield",
    tag = "yield",
    params(YieldQuery),
    responses((status = 200, body = [YieldAssetOut]))
)]
pub async fn yield_assets(
    State(st): State<AppState>,
    Query(q): Query<YieldQuery>,
) -> AppResult<Json<Arc<Vec<YieldAssetOut>>>> {
    Ok(Json(services::asset_yield::list(&st, q.chain_id).await?))
}
