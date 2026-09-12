//! The note-commitment chunk feed.
//!
//! One pre-hashed Merkle leaf per entry: hashing was the only thing a client did
//! with the raw `cm` / `cv_dep`, so serving the leaf cuts the largest feed in a
//! cold sync roughly threefold. Clients verify the root they build against the
//! on-chain root instead of re-deriving leaves.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::field::{bigdec_to_field, bytes_to_field, field_to_hex};
use crate::domain::poseidon::leaf_hash;
use crate::domain::responses::{CommitmentChunkOut, CommitmentEntry, RenderedChunk};
use crate::repositories::notes;
use crate::services::chunks;

pub use crate::services::chunks::CHUNK_SIZE;

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

/// One chunk of the commitment feed, serialised and ready to write.
///
/// A hit returns the bytes rendered when the chunk was first assembled: a
/// complete chunk never changes, so re-serialising 1024 entries per request
/// would recompute a constant.
pub async fn get_chunk(st: &AppState, chain_id: i64, chunk_id: u64) -> AppResult<RenderedChunk> {
    chunks::serve(
        &st.cache.chunks,
        "chunks",
        (chain_id, chunk_id),
        render(st, chain_id, chunk_id),
    )
    .await
}

async fn render(st: &AppState, chain_id: i64, chunk_id: u64) -> AppResult<RenderedChunk> {
    let (from, to) = chunks::range(chunk_id);
    let rows = notes::list_leaf_inputs(&st.pool, chain_id, from, to).await?;
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
            Ok(CommitmentEntry {
                leaf_index: r.leaf_index,
                leaf_hash: field_to_hex(&leaf_hash(&cm, &x, &y)?),
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    RenderedChunk::render(&CommitmentChunkOut {
        chunk_id,
        entries,
        is_complete,
    })
    .map_err(|e| AppError::Internal(format!("serialise commitment chunk: {e}")))
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
