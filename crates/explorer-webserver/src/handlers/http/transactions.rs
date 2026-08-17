use crate::app::AppState;
use crate::domain::dto::{self, RecentTxQuery, TxKindsQuery};
use crate::domain::error::AppResult;
use crate::domain::responses::{KindCounts, TxOut};
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/v1/transactions",
    tag = "transactions",
    params(RecentTxQuery),
    responses(
        (status = 200, body = [TxOut]),
        (status = 400, description = "kind is not a known transaction kind")
    )
)]
pub async fn recent_transactions(
    State(st): State<AppState>,
    Query(q): Query<RecentTxQuery>,
) -> AppResult<Json<Arc<Vec<TxOut>>>> {
    let limit = dto::page_limit(q.limit);
    let kind = dto::tx_kind(q.kind.as_deref())?;
    Ok(Json(
        services::transactions::recent(&st, q.chain_id, q.since_ts, kind, limit).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/tx-kinds",
    tag = "transactions",
    params(TxKindsQuery),
    responses(
        (status = 200, body = [KindCounts]),
        (status = 400, description = "bucketSec is not a positive multiple of 3600")
    )
)]
pub async fn tx_kinds(
    State(st): State<AppState>,
    Query(q): Query<TxKindsQuery>,
) -> AppResult<Json<Arc<Vec<KindCounts>>>> {
    let bucket = dto::bucket_sec(q.bucket_sec)?;
    Ok(Json(
        services::transactions::kind_counts(&st, q.chain_id, bucket, q.since_ts).await?,
    ))
}
