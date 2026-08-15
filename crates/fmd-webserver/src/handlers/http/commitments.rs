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
pub struct CommitmentEntry {
    pub leaf_index: i64,
    pub cm_hex: String,
    /// `0x`-prefixed 32-byte field elements for
    /// Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y).
    pub cv_dep_x: String,
    pub cv_dep_y: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitmentChunkOut {
    pub chunk_id: u64,
    pub entries: Vec<CommitmentEntry>,
    pub is_complete: bool,
}

#[utoipa::path(
    get,
    path = "/v1/chains/{chain_id}/commitments/chunks/{chunk_id}",
    tag = "commitments",
    params(
        ("chain_id" = i64, Path, description = "Chain id"),
        ("chunk_id" = u64, Path, description = "Chunk index (chunk_id * 1024 = first leaf_index in chunk)"),
    ),
    responses((status = 200, body = CommitmentChunkOut))
)]
#[tracing::instrument(skip(st), fields(chain_id, chunk_id))]
pub async fn get_commitment_chunk(
    State(st): State<AppState>,
    Path((chain_id, chunk_id)): Path<(i64, u64)>,
) -> AppResult<impl IntoResponse> {
    let chunk = services::commitments::get_chunk(&st, chain_id, chunk_id).await?;
    let out = CommitmentChunkOut {
        chunk_id: chunk.chunk_id,
        entries: chunk
            .entries
            .iter()
            .map(|e| CommitmentEntry {
                leaf_index: e.leaf_index,
                cm_hex: e.cm_hex.clone(),
                cv_dep_x: e.cv_dep_x.clone(),
                cv_dep_y: e.cv_dep_y.clone(),
            })
            .collect(),
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
