use crate::app::AppState;
use crate::domain::dto::ListAssetsQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::AssetOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/assets",
    tag = "assets",
    params(ListAssetsQuery),
    responses((status = 200, body = [AssetOut]))
)]
pub async fn list_assets(
    State(st): State<AppState>,
    Query(q): Query<ListAssetsQuery>,
) -> AppResult<Json<Arc<Vec<AssetOut>>>> {
    Ok(Json(services::assets::list(&st, q.chain_id).await?))
}
