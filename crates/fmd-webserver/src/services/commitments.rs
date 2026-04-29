use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::repositories::notes;
use std::sync::Arc;

pub const CHUNK_SIZE: u64 = 1024;

pub struct ChunkEntry {
    pub leaf_index: i64,
    pub cm_hex: String,
    /// Decimal field-element strings. Client uses these with Poseidon(TAG_LEAF, cm, x, y)
    /// to compute the Merkle leaf hash for local tree construction.
    pub cv_dep_x: String,
    pub cv_dep_y: String,
}

pub struct ChunkResponse {
    pub chunk_id: u64,
    pub entries: Vec<ChunkEntry>,
    pub is_complete: bool,
}

pub async fn get_chunk(st: &AppState, chain_id: i64, chunk_id: u64) -> AppResult<Arc<ChunkResponse>> {
    // Only complete (immutable) chunks are cached; see `AppCache::chunks`.
    if let Some(cached) = st.cache.chunks.get(&(chain_id, chunk_id)).await {
        return Ok(cached);
    }

    let from = (chunk_id * CHUNK_SIZE) as i64;
    let to = from + CHUNK_SIZE as i64;
    let rows = notes::list_chunk(&st.pool, chain_id, from, to).await?;
    let is_complete = rows.len() as u64 == CHUNK_SIZE;
    let entries = rows
        .into_iter()
        .map(|r| ChunkEntry {
            leaf_index: r.leaf_index,
            cm_hex: hex::encode(&r.cm),
            cv_dep_x: r.cv_dep_x.to_string(),
            cv_dep_y: r.cv_dep_y.to_string(),
        })
        .collect();
    let response = Arc::new(ChunkResponse { chunk_id, entries, is_complete });

    if is_complete {
        st.cache.chunks.insert((chain_id, chunk_id), Arc::clone(&response)).await;
    }

    Ok(response)
}
