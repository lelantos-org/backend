use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::repositories::nullifiers;
use std::sync::Arc;

pub const CHUNK_SIZE: u64 = 1024;

pub struct ChunkResponse {
    pub chunk_id: u64,
    /// 0x-prefixed hex, ascending by `seq`. The client holds the whole set
    /// and filters its own notes locally — the server never learns which
    /// nullifiers a wallet cares about.
    pub nullifiers: Vec<String>,
    pub is_complete: bool,
}

pub async fn get_chunk(
    st: &AppState,
    chain_id: i64,
    chunk_id: u64,
) -> AppResult<Arc<ChunkResponse>> {
    // Only complete (immutable) chunks are cached; see `AppCache::nullifier_chunks`.
    if let Some(cached) = st.cache.nullifier_chunks.get(&(chain_id, chunk_id)).await {
        return Ok(cached);
    }

    let from = (chunk_id * CHUNK_SIZE) as i64;
    let to = from + CHUNK_SIZE as i64;
    let rows = nullifiers::list_chunk(&st.pool, chain_id, from, to).await?;
    let is_complete = rows.len() as u64 == CHUNK_SIZE;
    let nullifiers = rows
        .into_iter()
        .map(|nf| format!("0x{}", hex::encode(nf)))
        .collect();
    let response = Arc::new(ChunkResponse {
        chunk_id,
        nullifiers,
        is_complete,
    });

    if is_complete {
        st.cache
            .nullifier_chunks
            .insert((chain_id, chunk_id), Arc::clone(&response))
            .await;
    }

    Ok(response)
}
