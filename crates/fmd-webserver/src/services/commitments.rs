use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::repositories::notes;
use crate::services::field::{bigdec_to_field, bytes_to_field, field_to_hex};
use crate::services::poseidon::leaf_hash;
use std::sync::Arc;

pub const CHUNK_SIZE: u64 = 1024;

pub struct ChunkEntry {
    pub leaf_index: i64,
    /// `Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`, precomputed.
    ///
    /// Sending the hash rather than `cm` and the two `cv_dep` coordinates is one
    /// field element instead of three: it saves each client 1,048,576 pure-JS
    /// Poseidon-4 calls over a full tree and cuts the feed roughly threefold.
    ///
    /// Clients cannot derive the leaf themselves, so they are expected to verify
    /// the root they build against the on-chain root.
    pub leaf_hash: String,
}

pub struct ChunkResponse {
    pub chunk_id: u64,
    pub entries: Vec<ChunkEntry>,
    pub is_complete: bool,
}

/// Reject a chunk whose `leaf_index` values are not `from, from+1, ...`.
///
/// The tree is positional: a hole shifts every later leaf by one, so the client
/// builds a root no wallet can verify and the failure surfaces later as a
/// rejected proof.
///
/// `services::tree` makes the same check for the mirror behind `/v1/tree-state`;
/// this check covers the feed clients build their tree from.
fn ensure_dense(rows: &[notes::LeafInputsRow], from: i64) -> AppResult<()> {
    for (i, row) in rows.iter().enumerate() {
        let expected = from + i as i64;
        if row.leaf_index != expected {
            return Err(AppError::Internal(format!(
                "commitment chunk is not dense: note has leaf_index {} (expected {expected})",
                row.leaf_index
            )));
        }
    }
    Ok(())
}

pub async fn get_chunk(
    st: &AppState,
    chain_id: i64,
    chunk_id: u64,
) -> AppResult<Arc<ChunkResponse>> {
    // Only complete (immutable) chunks are cached; see `AppCache::chunks`.
    let cached = st.cache.chunks.get(&(chain_id, chunk_id)).await;
    shared::metrics::record_cache("chunks", cached.is_some());
    if let Some(cached) = cached {
        return Ok(cached);
    }

    let from = (chunk_id * CHUNK_SIZE) as i64;
    let to = from + CHUNK_SIZE as i64;
    let rows = notes::list_leaf_inputs(&st.pool, chain_id, from, Some(to)).await?;
    let is_complete = rows.len() as u64 == CHUNK_SIZE;
    ensure_dense(&rows, from)?;
    let entries = rows
        .into_iter()
        .map(|r| {
            // `cm` is BYTEA and the coordinates are NUMERIC; both convert to the
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

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;

    fn row(leaf_index: i64) -> notes::LeafInputsRow {
        notes::LeafInputsRow {
            leaf_index,
            cm: vec![0u8; 32],
            cv_dep_x: BigDecimal::from(1),
            cv_dep_y: BigDecimal::from(2),
        }
    }

    #[test]
    fn accepts_a_dense_run() {
        let rows: Vec<_> = (1024..1027).map(row).collect();
        assert!(ensure_dense(&rows, 1024).is_ok());
    }

    #[test]
    fn accepts_an_empty_chunk() {
        // Past the end of the tree: not a gap, simply empty.
        assert!(ensure_dense(&[], 4096).is_ok());
    }

    #[test]
    fn rejects_a_gap() {
        let rows = vec![row(0), row(2)];
        let err = ensure_dense(&rows, 0).unwrap_err().to_string();
        assert!(err.contains("leaf_index 2"), "{err}");
        assert!(err.contains("expected 1"), "{err}");
    }

    #[test]
    fn rejects_a_chunk_that_does_not_start_at_its_own_boundary() {
        // A short first page would otherwise be served as if it began at the
        // chunk boundary, shifting every leaf in it.
        let rows = vec![row(1025)];
        assert!(ensure_dense(&rows, 1024).is_err());
    }
}
