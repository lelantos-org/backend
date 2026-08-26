//! Tree-mirror service for `/v1/tree-state`.
//!
//! The canonical merkle tree mirrors the `notes` table ordered by `leaf_index`
//! and is held per chain in `AppState.cache.tree`. Notes are append-only, so the
//! mirror advances in place and each request hashes only the leaves added since
//! the last one. Only a reorg can invalidate what is already there, and it is
//! detected by the tip moving backwards.

use crate::app::AppState;
use crate::app::cache::TreeMirror;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::TreeStateOut;
use crate::repositories::notes;
use crate::services::field::{bigdec_to_field, field_to_hex};
use crate::services::poseidon::leaf_hash;
use database::DbPool;
use fmd_crypto::tree::{Field, MerkleTree};
use std::sync::Arc;
use tokio::sync::Mutex;

const DEPTH: usize = 10;

fn vec_to_field(v: &[u8]) -> AppResult<Field> {
    fmd_crypto::tree::field_from_bytes(v).map_err(|e| AppError::Internal(e.to_string()))
}

/// Hash `rows` into leaves and append them to `tree`.
///
/// `rows` must start exactly at the current leaf count and be contiguous. The
/// tree is positional, so a gap would shift every later leaf and produce a root
/// no wallet could verify.
fn append_leaves(tree: &mut MerkleTree, rows: &[notes::LeafInputsRow]) -> AppResult<()> {
    let base = tree.leaf_count() as i64;
    let mut leaves = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let expected = base + i as i64;
        if row.leaf_index != expected {
            return Err(AppError::Internal(format!(
                "tree desynced: note has leaf_index {} (expected {})",
                row.leaf_index, expected
            )));
        }
        let cm_f = vec_to_field(&row.cm)?;
        let cv_x = bigdec_to_field(&row.cv_dep_x)?;
        let cv_y = bigdec_to_field(&row.cv_dep_y)?;
        leaves.push(leaf_hash(&cm_f, &cv_x, &cv_y)?);
    }
    tree.extend(leaves)
        .map_err(|e| AppError::Internal(e.to_string()))
}

/// Count a rebuild from leaf 0 under the cause that forced it.
///
/// The `reason` dimension carries the signal: `tip_backwards` is a reorg the
/// mirror survived, while `cold` is a full re-hash a warm mirror should not pay
/// again. A bare total cannot distinguish them.
fn record_rebuild(chain_id: i64, reason: &'static str) {
    metrics::counter!(
        shared::metrics::name::TREE_MIRROR_REBUILDS,
        "chain_id" => chain_id.to_string(),
        "reason" => reason,
    )
    .increment(1);
}

/// Bring this chain's mirror up to the current tip, hashing only new leaves.
async fn sync_mirror(pool: &DbPool, chain_id: i64, mirror: &mut TreeMirror) -> AppResult<()> {
    let started = std::time::Instant::now();
    let tip = notes::max_leaf_index(pool, chain_id).await?;
    let db_leaves = tip.map_or(0, |t| t + 1);
    let have = mirror.tree.leaf_count() as i64;

    // Only a reorg can remove leaves, and appending cannot repair the mirror in
    // that case, so it is dropped and rebuilt from leaf 0.
    if db_leaves < have {
        tracing::warn!(
            chain_id,
            have,
            db_leaves,
            "notes tip moved backwards; rebuilding tree mirror"
        );
        record_rebuild(chain_id, "tip_backwards");
        mirror.tree = MerkleTree::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
    } else if have == 0 && db_leaves > 0 {
        // An empty mirror with leaves to fold is the cold build: every leaf is
        // hashed while holding the chain's mutex. The boot warm-up exists to move
        // this off the request path.
        record_rebuild(chain_id, "cold");
    }

    let from = mirror.tree.leaf_count() as i64;
    // Published before the early return. This is a state gauge and the steady
    // state is having nothing to append, so setting it only on the work path
    // would make the series vanish for a caught-up chain.
    metrics::gauge!(
        shared::metrics::name::TREE_MIRROR_LEAVES,
        "chain_id" => chain_id.to_string(),
    )
    .set(from as f64);
    if from >= db_leaves {
        return Ok(());
    }

    let rows = notes::list_leaf_inputs(pool, chain_id, from, None).await?;
    append_leaves(&mut mirror.tree, &rows)?;
    tracing::debug!(
        chain_id,
        appended = rows.len(),
        leaf_count = mirror.tree.leaf_count(),
        "tree mirror advanced"
    );

    // Timed only on a path that did work: recording the no-op return above would
    // bury the multi-second cold builds under a flood of zeros. Unlike the
    // histogram, the gauge is re-set here to the post-append count.
    let chain = chain_id.to_string();
    metrics::histogram!(
        shared::metrics::name::TREE_MIRROR_SYNC_DURATION,
        "chain_id" => chain.clone(),
    )
    .record(started.elapsed().as_secs_f64());
    metrics::gauge!(
        shared::metrics::name::TREE_MIRROR_LEAVES,
        "chain_id" => chain,
    )
    .set(mirror.tree.leaf_count() as f64);
    Ok(())
}

/// The chain's mirror, created empty on first use. Callers lock it and call
/// [`sync_mirror`] before reading.
async fn mirror(st: &AppState, chain_id: i64) -> AppResult<Arc<Mutex<TreeMirror>>> {
    let probe = shared::metrics::CacheProbe::new("tree");
    let miss = probe.marker();
    let out = st
        .cache
        .tree
        .try_get_with(chain_id, async move {
            miss.mark();
            let tree = MerkleTree::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
            Ok::<_, AppError>(Arc::new(Mutex::new(TreeMirror { tree })))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()));
    probe.record();
    out
}

#[tracing::instrument(skip(st))]
pub async fn tree_state(st: &AppState, chain_id: i64) -> AppResult<TreeStateOut> {
    let cell = mirror(st, chain_id).await?;
    let mut guard = cell.lock().await;
    sync_mirror(&st.pool, chain_id, &mut guard).await?;

    let tree = &guard.tree;
    let root = tree.root().map_err(|e| AppError::Internal(e.to_string()))?;
    let frontier = tree
        .frontier()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(TreeStateOut {
        chain_id,
        leaf_count: tree.leaf_count() as i64,
        root_hex: field_to_hex(&root),
        frontier_hex: frontier
            .iter()
            .map(|row| row.iter().map(field_to_hex).collect())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;

    fn row(leaf_index: i64) -> notes::LeafInputsRow {
        let mut cm = [0u8; 32];
        cm[24..].copy_from_slice(&(leaf_index as u64).to_be_bytes());
        notes::LeafInputsRow {
            leaf_index,
            cm: cm.to_vec(),
            cv_dep_x: BigDecimal::from(leaf_index + 1),
            cv_dep_y: BigDecimal::from(leaf_index + 2),
        }
    }

    fn tree_of(rows: &[notes::LeafInputsRow]) -> MerkleTree {
        let mut t = MerkleTree::new(DEPTH).unwrap();
        append_leaves(&mut t, rows).unwrap();
        t
    }

    /// Appending in slices must land on the same root as hashing every leaf in
    /// one pass.
    #[test]
    fn incremental_append_matches_a_full_build() {
        let all: Vec<_> = (0..40).map(row).collect();
        let full = tree_of(&all);

        let mut incremental = MerkleTree::new(DEPTH).unwrap();
        for chunk in all.chunks(7) {
            append_leaves(&mut incremental, chunk).unwrap();
        }

        assert_eq!(incremental.leaf_count(), full.leaf_count());
        assert_eq!(incremental.root().unwrap(), full.root().unwrap());
        assert_eq!(incremental.frontier().unwrap(), full.frontier().unwrap());
    }

    #[test]
    fn append_rejects_a_gap() {
        let mut t = MerkleTree::new(DEPTH).unwrap();
        append_leaves(&mut t, &[row(0), row(1)]).unwrap();

        // leaf_index 3 while the tree holds 2 leaves: a missing note would shift
        // every later leaf, so this must fail.
        let err = append_leaves(&mut t, &[row(3)]).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)), "got {err:?}");
        assert_eq!(t.leaf_count(), 2);
    }

    #[test]
    fn append_rejects_a_replayed_leaf() {
        let mut t = MerkleTree::new(DEPTH).unwrap();
        append_leaves(&mut t, &[row(0), row(1)]).unwrap();
        assert!(append_leaves(&mut t, &[row(1)]).is_err());
    }
}
