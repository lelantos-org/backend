use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::services;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::IntoResponse;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NullifierChunkOut {
    pub chunk_id: u64,
    /// 0x-prefixed hex, 32 bytes each, ascending by insertion order.
    pub nullifiers: Vec<String>,
    /// `false` marks the tail chunk — the client stops paging here.
    pub is_complete: bool,
}

#[utoipa::path(
    get,
    path = "/v1/chains/{chain_id}/nullifiers/chunks/{chunk_id}",
    tag = "nullifiers",
    params(
        ("chain_id" = i64, Path, description = "Chain id"),
        ("chunk_id" = u64, Path, description = "Chunk index (chunk_id * 1024 = first seq in chunk)"),
    ),
    responses((status = 200, body = NullifierChunkOut))
)]
#[tracing::instrument(skip(st), fields(chain_id, chunk_id))]
pub async fn get_nullifier_chunk(
    State(st): State<AppState>,
    Path((chain_id, chunk_id)): Path<(i64, u64)>,
) -> AppResult<impl IntoResponse> {
    let chunk = services::nullifiers::get_chunk(&st, chain_id, chunk_id).await?;
    let out = NullifierChunkOut {
        chunk_id: chunk.chunk_id,
        nullifiers: chunk.nullifiers.clone(),
        is_complete: chunk.is_complete,
    };
    let cache = if out.is_complete {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=5"
    };
    let mut resp = Json(out).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Ok(resp)
}
