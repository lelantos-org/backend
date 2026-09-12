use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::AssetOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ListAssetsQuery {
    /// Restrict to one chain. Omitted returns every chain's assets.
    pub chain_id: Option<i64>,
}

/// Every registered asset, with its yield state and rate estimate.
///
/// An empty list means the indexer has not caught up rather than that the chain
/// supports no assets.
#[utoipa::path(
    get,
    path = "/v1/assets",
    tag = "registry",
    params(ListAssetsQuery),
    responses((status = 200, body = Vec<AssetOut>))
)]
pub async fn list_assets(
    State(st): State<AppState>,
    Query(q): Query<ListAssetsQuery>,
) -> AppResult<Json<Arc<Vec<AssetOut>>>> {
    Ok(Json(services::assets::list(&st, q.chain_id).await?))
}
