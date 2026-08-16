use crate::app::AppState;
use crate::domain::dto::{self, AssetFlowsQuery};
use crate::domain::error::AppResult;
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
    let bucket = dto::bucket_sec(q.bucket_sec)?;
    Ok(Json(
        services::asset_flows::flows(&st, q.chain_id, q.asset_id_u64, bucket, q.since_ts).await?,
    ))
}
