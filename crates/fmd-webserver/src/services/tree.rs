// Tree-mirror service for `/v1/path/:cm` and `/v1/tree-state`.
//
// The canonical merkle tree is rebuilt from the `notes` table (ordered by
// leaf_index) and cached per chain in `AppState.cache.tree`. Concurrent
// requests during a cache miss share the same load via moka `try_get_with`,
// so only one rebuild runs at a time. Entries expire on a short TTL.

use crate::app::AppState;
use crate::app::cache::TreeSnapshot;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{MerkleProofOut, TreeStateOut};
use crate::repositories::notes;
use crate::services::poseidon::{leaf_hash, recompute_root};
use database::DbPool;
use fmd_crypto::tree::{Field, MerkleTree};
use std::sync::Arc;

fn bigdec_to_field(v: &bigdecimal::BigDecimal) -> AppResult<Field> {
    use bigdecimal::num_bigint::Sign;
    let (bi, _) = v.as_bigint_and_exponent();
    if bi.sign() == Sign::Minus {
        return Err(AppError::Internal("negative cv_dep".into()));
    }
    let bytes = bi.to_bytes_be().1;
    if bytes.len() > 32 {
        return Err(AppError::Internal("cv_dep > 32 bytes".into()));
    }
    let mut f = [0u8; 32];
    f[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(f)
}

const DEPTH: usize = 10;

fn vec_to_field(v: &[u8]) -> AppResult<Field> {
    if v.len() != 32 {
        return Err(AppError::Internal(format!(
            "expected 32-byte field, got {}",
            v.len()
        )));
    }
    let mut f = [0u8; 32];
    f.copy_from_slice(v);
    Ok(f)
}

fn field_to_hex(f: &Field) -> String {
    format!("0x{}", hex::encode(f))
}

#[tracing::instrument(skip(pool))]
async fn build_tree(pool: &DbPool, chain_id: i64) -> AppResult<MerkleTree> {
    let mut tree = MerkleTree::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
    let rows = notes::list_leaf_inputs_by_leaf(pool, chain_id).await?;
    for (i, row) in rows.iter().enumerate() {
        if row.leaf_index != i as i64 {
            return Err(AppError::Internal(format!(
                "tree desynced: notes row {} has leaf_index {} (expected {})",
                i, row.leaf_index, i
            )));
        }
        let cm_f = vec_to_field(&row.cm)?;
        let cv_x = bigdec_to_field(&row.cv_dep_x)?;
        let cv_y = bigdec_to_field(&row.cv_dep_y)?;
        let leaf = leaf_hash(&cm_f, &cv_x, &cv_y)?;
        tree.insert(leaf);
    }
    tracing::debug!(leaf_count = tree.leaf_count(), "tree built");
    Ok(tree)
}

async fn snapshot(st: &AppState, chain_id: i64) -> AppResult<Arc<TreeSnapshot>> {
    let pool = st.pool.clone();
    st.cache
        .tree
        .try_get_with(chain_id, async move {
            let tree = build_tree(&pool, chain_id).await?;
            Ok::<_, AppError>(Arc::new(TreeSnapshot { tree }))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

#[tracing::instrument(skip(st))]
pub async fn tree_state(st: &AppState, chain_id: i64) -> AppResult<TreeStateOut> {
    let snap = snapshot(st, chain_id).await?;
    let tree = &snap.tree;
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

#[tracing::instrument(skip(st, cm), fields(cm = %hex::encode(cm)))]
pub async fn path(st: &AppState, chain_id: i64, cm: &[u8]) -> AppResult<MerkleProofOut> {
    let row = notes::find_leaf_inputs_by_cm(&st.pool, chain_id, cm)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("cm {} not found", hex::encode(cm))))?;
    let leaf_index = row.leaf_index;
    let cm_f = vec_to_field(cm)?;
    let cv_x = bigdec_to_field(&row.cv_dep_x)?;
    let cv_y = bigdec_to_field(&row.cv_dep_y)?;
    let leaf = leaf_hash(&cm_f, &cv_x, &cv_y)?;

    let snap = snapshot(st, chain_id).await?;
    let proof = snap
        .tree
        .proof(leaf_index as usize)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let root = recompute_root(&leaf, &proof.path_elements, &proof.path_indices)?;

    Ok(MerkleProofOut {
        leaf_index,
        commitment_hex: format!("0x{}", hex::encode(cm)),
        path_elements_hex: proof
            .path_elements
            .iter()
            .map(|row| row.iter().map(field_to_hex).collect())
            .collect(),
        path_indices: proof.path_indices,
        root_hex: field_to_hex(&root),
    })
}
