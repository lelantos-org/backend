use crate::app::AppState;
use crate::domain::dto::{ListTreeAdvancesQuery, TxCountsQuery};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{ChainFlowOut, CountPoint, TreeAdvanceOut};
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/tree-advances",
    tag = "tree-advances",
    params(ListTreeAdvancesQuery),
    responses((status = 200, body = [TreeAdvanceOut]))
)]
pub async fn list_tree_advances(
    State(st): State<AppState>,
    Query(q): Query<ListTreeAdvancesQuery>,
) -> AppResult<Json<Arc<Vec<TreeAdvanceOut>>>> {
    let limit = q.limit.unwrap_or(100).min(1000);
    Ok(Json(
        services::tree_advances::list(&st, q.chain_id, q.since_start_index, limit).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/tx-counts",
    tag = "tree-advances",
    params(TxCountsQuery),
    responses((status = 200, body = [CountPoint]))
)]
pub async fn tx_counts(
    State(st): State<AppState>,
    Query(q): Query<TxCountsQuery>,
) -> AppResult<Json<Arc<Vec<CountPoint>>>> {
    let bucket = q.bucket_sec.unwrap_or(3600);
    if bucket <= 0 || bucket % 3600 != 0 {
        return Err(AppError::BadRequest(
            "bucketSec must be a positive multiple of 3600".into(),
        ));
    }
    Ok(Json(
        services::tree_advances::tx_counts(&st, q.chain_id, bucket, q.since_ts).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/chain-flows-24h",
    tag = "tree-advances",
    responses((status = 200, body = [ChainFlowOut]))
)]
pub async fn chain_flows_24h(
    State(st): State<AppState>,
) -> AppResult<Json<Arc<Vec<ChainFlowOut>>>> {
    let now_ts = chrono::Utc::now().timestamp();
    Ok(Json(
        services::tree_advances::chain_flows_24h(&st, now_ts).await?,
    ))
}
