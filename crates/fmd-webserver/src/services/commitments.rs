use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::repositories::notes;
use crate::services::field::{bigdec_to_field, bytes_to_field, field_to_hex};
use crate::services::poseidon::leaf_hash;
use std::sync::Arc;

pub const CHUNK_SIZE: u64 = 1024;

pub struct ChunkEntry {
    pub leaf_index: i64,
    /// `Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`, precomputed.
    ///
    /// This feed used to carry `cm` and the two `cv_dep` coordinates, and the
    /// only thing any client did with them was hash them into this. Sending
    /// the result is one field element instead of three: it saves each client
    /// 1,048,576 pure-JS Poseidon-4 calls over a full tree and cuts the feed
    /// roughly threefold.
    ///
    /// Clients can no longer derive the leaf themselves, so they are expected
    /// to verify the root they build against the on-chain root.
    pub leaf_hash: String,
}

pub struct ChunkResponse {
    pub chunk_id: u64,
    pub entries: Vec<ChunkEntry>,
    pub is_complete: bool,
}

pub async fn get_chunk(
    st: &AppState,
    chain_id: i64,
    chunk_id: u64,
) -> AppResult<Arc<ChunkResponse>> {
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
        .map(|r| {
            // `cm` is BYTEA, the coordinates are NUMERIC; both become the same
            // 32-byte big-endian form the leaf hash takes.
            let cm = bytes_to_field(&r.cm)?;
            let x = bigdec_to_field(&r.cv_dep_x)?;
            let y = bigdec_to_field(&r.cv_dep_y)?;
            Ok(ChunkEntry {
                leaf_index: r.leaf_index,
                leaf_hash: field_to_hex(&leaf_hash(&cm, &x, &y)?),
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    let response = Arc::new(ChunkResponse {
        chunk_id,
        entries,
        is_complete,
    });

    if is_complete {
        st.cache
            .chunks
            .insert((chain_id, chunk_id), Arc::clone(&response))
            .await;
    }

    Ok(response)
}
