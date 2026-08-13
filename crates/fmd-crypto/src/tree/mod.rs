// Quaternary sparse Merkle tree, Poseidon-arity-5 nodes:
//   node = Poseidon(TAG_MERKLE, c0, c1, c2, c3)
//
// Rust port of `sdk/src/crypto/merkle.ts`. MUST stay byte-identical to the
// SDK and to `circuits/src/lib/merkle.circom`. Used by:
//   - fmd-webserver: serve `/v1/tree-state`.
//   - relayer: build tree_update witnesses (frontier + path indices).
//
// NOT a privacy-sensitive primitive — the tree is public data — but lives in
// `fmd-crypto` because it shares the Poseidon dependency and is only consumed
// by FMD-zone crates (explorer-* never references it).

mod hash;

use hash::{ARITY, hash_node};
use rayon::prelude::*;
use thiserror::Error;

pub use hash::TAG_MERKLE;

#[derive(Debug, Error)]
pub enum TreeError {
    #[error("poseidon: {0}")]
    Poseidon(String),
    #[error("leaf index {0} out of range")]
    OutOfRange(usize),
}

/// Big-endian 32-byte field element (matches SDK `Field`).
pub type Field = [u8; 32];

#[derive(Debug, Clone)]
pub struct MerkleProof {
    pub path_elements: Vec<[Field; 3]>,
    pub path_indices: Vec<u8>,
}

pub struct MerkleTree {
    pub depth: usize,
    /// Materialised nodes per level: `levels[0]` are the leaves, `levels[d][i]`
    /// is the node at level `d`, index `i`. Indices past a level's length are
    /// implicitly `zeros[d]` — since `zeros[d+1] = hash(zeros[d] × 4)`, an
    /// absent node and a materialised all-zero subtree are the same value.
    ///
    /// Kept in sync by every mutation, so `root()` is O(1) and `frontier()` /
    /// `proof()` are O(depth) table lookups. Costs ~1.33 × leaf-count of
    /// memory; buys back the O(N) Poseidon sweep the recursive form paid on
    /// every single call.
    levels: Vec<Vec<Field>>,
    zeros: Vec<Field>,
}

impl MerkleTree {
    pub fn new(depth: usize) -> Result<Self, TreeError> {
        let mut zeros: Vec<Field> = Vec::with_capacity(depth + 1);
        let mut z: Field = [0u8; 32];
        for _ in 0..depth {
            zeros.push(z);
            z = hash_node(&z, &z, &z, &z)?;
        }
        zeros.push(z);
        Ok(Self {
            depth,
            levels: vec![Vec::new(); depth + 1],
            zeros,
        })
    }

    /// Append one leaf and refresh the `depth` nodes on its path to the root.
    pub fn insert(&mut self, leaf: Field) -> Result<usize, TreeError> {
        self.levels[0].push(leaf);
        let index = self.levels[0].len() - 1;
        self.refresh_path(index)?;
        Ok(index)
    }

    /// Append many leaves and rebuild the internal levels bottom-up in
    /// parallel. O(N) total rather than the O(N · depth) of N `insert` calls —
    /// use this for bootstrap replay.
    pub fn extend(&mut self, leaves: impl IntoIterator<Item = Field>) -> Result<(), TreeError> {
        self.levels[0].extend(leaves);
        self.rebuild()
    }

    pub fn leaf_count(&self) -> usize {
        self.levels[0].len()
    }

    /// Drop the last `n` leaves. Used by callers that speculatively
    /// `insert` and need to undo on failure (relayer rollback).
    ///
    /// Every level shrinks to `ceil(child_len / ARITY)`; only its new last
    /// node can have lost children, so one re-hash per level suffices.
    pub fn truncate_leaves(&mut self, n: usize) -> Result<(), TreeError> {
        let len = self.levels[0].len();
        let keep = len.saturating_sub(n);
        if keep == len {
            return Ok(());
        }
        self.levels[0].truncate(keep);
        for lvl in 0..self.depth {
            let parent_len = self.levels[lvl].len().div_ceil(ARITY);
            self.levels[lvl + 1].truncate(parent_len);
            if parent_len > 0 {
                let parent = parent_len - 1;
                let first = parent * ARITY;
                self.levels[lvl + 1][parent] = hash_node(
                    &self.node_at(lvl, first),
                    &self.node_at(lvl, first + 1),
                    &self.node_at(lvl, first + 2),
                    &self.node_at(lvl, first + 3),
                )?;
            }
        }
        Ok(())
    }

    pub fn root(&self) -> Result<Field, TreeError> {
        Ok(self.node_at(self.depth, 0))
    }

    /// Retained for call sites that predate incremental node maintenance.
    /// `root()` is now O(1), so this is a plain alias.
    pub fn root_par(&self) -> Result<Field, TreeError> {
        self.root()
    }

    /// Re-hash the path from `leaf_index` up to the root. Each level's parent
    /// index grows by at most one slot, so the `resize` appends at most one
    /// entry and never leaves a gap.
    fn refresh_path(&mut self, leaf_index: usize) -> Result<(), TreeError> {
        let mut idx = leaf_index;
        for lvl in 0..self.depth {
            let parent = idx / ARITY;
            let first = parent * ARITY;
            let h = hash_node(
                &self.node_at(lvl, first),
                &self.node_at(lvl, first + 1),
                &self.node_at(lvl, first + 2),
                &self.node_at(lvl, first + 3),
            )?;
            let up = lvl + 1;
            if self.levels[up].len() <= parent {
                self.levels[up].resize(parent + 1, self.zeros[up]);
            }
            self.levels[up][parent] = h;
            idx = parent;
        }
        Ok(())
    }

    /// Rebuild every internal level from `levels[0]`, hashing each level's
    /// nodes across threads via rayon.
    fn rebuild(&mut self) -> Result<(), TreeError> {
        for lvl in 0..self.depth {
            let zero = self.zeros[lvl];
            let parents = {
                let child = &self.levels[lvl];
                let parent_len = child.len().div_ceil(ARITY);
                (0..parent_len)
                    .into_par_iter()
                    .map(|p| {
                        let f = p * ARITY;
                        let at = |i: usize| child.get(i).unwrap_or(&zero);
                        hash_node(at(f), at(f + 1), at(f + 2), at(f + 3))
                    })
                    .collect::<Result<Vec<Field>, TreeError>>()?
            };
            self.levels[lvl + 1] = parents;
        }
        Ok(())
    }

    /// `frontier()` — depth × 3 slots, mirroring the layout the prior on-chain
    /// `filledSubtrees` storage exposed. For each level lvl and slot k:
    ///   frontier[lvl][k] = nodeAt(lvl, parentIdx*4 + k)   if k < currentSlot
    ///   frontier[lvl][k] = 0                                otherwise
    /// where `currentSlot = (N / 4^lvl) % 4`, `parentIdx = N / 4^(lvl+1)`,
    /// `N = leaves.len()`. The k ≥ currentSlot entries are unread by the next
    /// insert at this level (would have been stale-from-prior-parent in the
    /// contract); we zero them deterministically.
    pub fn frontier(&self) -> Result<Vec<[Field; 3]>, TreeError> {
        let n = self.leaf_count();
        let mut out: Vec<[Field; 3]> = Vec::with_capacity(self.depth);
        for lvl in 0..self.depth {
            let stride = ARITY.pow(lvl as u32);
            let slot = (n / stride) % ARITY;
            let parent_idx = n / (stride * ARITY);
            let mut row = [[0u8; 32]; 3];
            #[allow(clippy::needless_range_loop)]
            for k in 0..3 {
                if k < slot {
                    row[k] = self.node_at(lvl, parent_idx * ARITY + k);
                }
            }
            out.push(row);
        }
        Ok(out)
    }

    /// Quaternary digits of `leaf_index`, level 0 (LSB) to `depth - 1`.
    pub fn path_indices_at(&self, leaf_index: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.depth);
        let mut idx = leaf_index;
        for _ in 0..self.depth {
            out.push((idx % ARITY) as u8);
            idx /= ARITY;
        }
        out
    }

    /// Sibling path for `leaf_index`. Not-yet-inserted leaves get zero
    /// siblings via `node_at`'s zero fallback, matching SDK behaviour.
    pub fn proof(&self, leaf_index: usize) -> Result<MerkleProof, TreeError> {
        let mut path_elements: Vec<[Field; 3]> = Vec::with_capacity(self.depth);
        let mut path_indices: Vec<u8> = Vec::with_capacity(self.depth);
        let mut idx = leaf_index;
        for level in 0..self.depth {
            let self_pos = idx % ARITY;
            let parent_idx = idx / ARITY;
            let mut sibs = [[0u8; 32]; 3];
            let mut s = 0usize;
            for k in 0..ARITY {
                if k != self_pos {
                    sibs[s] = self.node_at(level, parent_idx * ARITY + k);
                    s += 1;
                }
            }
            path_elements.push(sibs);
            path_indices.push(self_pos as u8);
            idx = parent_idx;
        }
        Ok(MerkleProof {
            path_elements,
            path_indices,
        })
    }

    /// Materialised node, or the level's zero-subtree constant when the index
    /// is past what has been filled.
    fn node_at(&self, level: usize, index: usize) -> Field {
        self.levels[level]
            .get(index)
            .copied()
            .unwrap_or(self.zeros[level])
    }
}

#[cfg(test)]
mod tests;
