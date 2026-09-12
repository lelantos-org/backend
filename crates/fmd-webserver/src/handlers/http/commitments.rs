use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::CommitmentChunkOut;
use crate::services;
use axum::extract::{Path, State};
use axum::response::IntoResponse;

/// The route template, shared by the router and the OpenAPI annotation below.
///
/// axum 0.8 and OpenAPI spell a path parameter the same way, `{chunk_id}`, so
/// one constant serves both and the two cannot drift apart.
pub const ROUTE: &str = "/v1/chains/{chain_id}/commitments/chunks/{chunk_id}";

#[utoipa::path(
    get,
    path = ROUTE,
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
    // The body arrives serialised, and carries its own `Cache-Control`: see
    // `domain::responses::RenderedChunk`.
    let chunk = services::commitments::get_chunk(&st, chain_id, chunk_id).await?;
    shared::metrics::record_chunk_feed_bytes("commitments", chunk.byte_len());
    Ok(chunk)
}
