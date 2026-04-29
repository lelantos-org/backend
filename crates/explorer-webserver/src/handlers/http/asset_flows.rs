use crate::app::AppState;
use crate::domain::dto::AssetFlowsQuery;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::FlowPoint;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/asset-flows",
    tag = "asset-flows",
    params(AssetFlowsQuery),
    responses((status = 200, body = [FlowPoint]))
)]
pub async fn asset_flows(
    State(st): State<AppState>,
    Query(q): Query<AssetFlowsQuery>,
) -> AppResult<Json<Arc<Vec<FlowPoint>>>> {
    let bucket = q.bucket_sec.unwrap_or(3600);
    if bucket <= 0 || bucket % 3600 != 0 {
        return Err(AppError::BadRequest(
            "bucketSec must be a positive multiple of 3600".into(),
        ));
    }
    Ok(Json(
        services::asset_flows::flows(&st, q.chain_id, q.asset_id_u64, bucket, q.since_ts).await?,
    ))
}
