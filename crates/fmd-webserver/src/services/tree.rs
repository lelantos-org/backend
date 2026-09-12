//! Tree state for `/v1/tree-state`.
//!
//! Read straight out of `tree_state`, which fmd-indexer advances as it commits
//! leaves. This process holds no tree.
//!
//! It used to. Every replica mirrored the whole quaternary tree in memory and
//! hashed every row of `notes` into it, which is ~180 MB per chain at the tree's
//! capacity, paid the O(N) Poseidon cold build on the first request after a boot
//! or a cache eviction, and could not be repaired at all once `notes` had a
//! `leaf_index` hole -- which the indexer legitimately produces for a leaf whose
//! ciphertext is too short to decode. The indexer sees the leaf the contract
//! actually inserted, so it can fold a hole that no reader of `notes` can.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::field::field_to_hex;
use crate::domain::responses::TreeStateOut;
use crate::repositories::tree_state;
use common_crypto::tree::{DEPTH, Frontier, decode_frontier, field_from_bytes};

#[tracing::instrument(skip(st))]
pub async fn tree_state(st: &AppState, chain_id: i64) -> AppResult<TreeStateOut> {
    match tree_state::load(&st.pool, chain_id).await? {
        Some(row) => out_of(chain_id, row.leaf_count, &row.root, &row.frontier),
        None => empty(chain_id),
    }
}

/// Map a stored row onto the wire form, rejecting a root or frontier that is not
/// the width `DEPTH` implies.
///
/// Checked rather than assumed: the writer is a different process, and a
/// truncated frontier would otherwise be served as a well-formed tree state for a
/// tree that never existed, which no client can detect.
fn out_of(chain_id: i64, leaf_count: i64, root: &[u8], frontier: &[u8]) -> AppResult<TreeStateOut> {
    let root =
        field_from_bytes(root).map_err(|e| AppError::Internal(format!("stored root: {e}")))?;
    let frontier = decode_frontier(DEPTH, frontier)
        .map_err(|e| AppError::Internal(format!("stored frontier: {e}")))?;
    Ok(TreeStateOut {
        chain_id,
        leaf_count,
        root_hex: field_to_hex(&root),
        frontier_hex: hex_rows(&frontier),
    })
}

/// A chain the indexer has not written yet: an empty tree, not an error.
///
/// The root is `zeros[DEPTH]` rather than 32 zero bytes -- an empty quaternary
/// tree still hashes its way up -- so this is derived from a `Frontier` instead
/// of written out, which keeps it correct if `DEPTH` ever moves.
fn empty(chain_id: i64) -> AppResult<TreeStateOut> {
    let empty = Frontier::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(TreeStateOut {
        chain_id,
        leaf_count: 0,
        root_hex: field_to_hex(&empty.root()),
        frontier_hex: hex_rows(&empty.slots()),
    })
}

fn hex_rows(frontier: &[[common_crypto::tree::Field; 3]]) -> Vec<Vec<String>> {
    frontier
        .iter()
        .map(|row| row.iter().map(field_to_hex).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use common_crypto::tree::encode_frontier;

    /// A tree with no leaves still has a root: `zeros[DEPTH]`, the fold of the
    /// empty subtree up every level. Serving 32 zero bytes instead would be a
    /// root no wallet can reproduce.
    #[test]
    fn an_absent_row_is_an_empty_tree_not_a_zero_root() {
        let out = empty(7).unwrap();
        assert_eq!(out.chain_id, 7);
        assert_eq!(out.leaf_count, 0);
        assert_ne!(out.root_hex, format!("0x{}", "00".repeat(32)));
        assert_eq!(out.frontier_hex.len(), DEPTH);
        assert!(out.frontier_hex.iter().all(|row| row.len() == 3));
    }

    /// What the indexer writes must come back out unchanged, in the layout the
    /// SDK reads. This is the whole contract between the two processes.
    #[test]
    fn a_stored_row_round_trips_to_the_wire_form() {
        let mut f = Frontier::new(DEPTH).unwrap();
        for i in 0..37u64 {
            let mut leaf = [0u8; 32];
            leaf[24..].copy_from_slice(&i.to_be_bytes());
            f.push(leaf).unwrap();
        }
        let out = out_of(1, 37, &f.root(), &encode_frontier(&f.slots())).unwrap();

        assert_eq!(out.leaf_count, 37);
        assert_eq!(out.root_hex, field_to_hex(&f.root()));
        assert_eq!(out.frontier_hex, hex_rows(&f.slots()));
        assert_eq!(out.frontier_hex.len(), DEPTH);
    }

    #[test]
    fn a_frontier_of_the_wrong_width_is_rejected() {
        let root = [0u8; 32];
        assert!(out_of(1, 0, &root, &vec![0u8; DEPTH * 3 * 32 - 1]).is_err());
        assert!(out_of(1, 0, &root, &[]).is_err());
    }

    #[test]
    fn a_root_of_the_wrong_width_is_rejected() {
        let frontier = vec![0u8; DEPTH * 3 * 32];
        assert!(out_of(1, 0, &[0u8; 31], &frontier).is_err());
    }
}
