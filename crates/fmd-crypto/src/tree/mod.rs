// Quaternary sparse Merkle tree, Poseidon-arity-5 nodes:
//   node = Poseidon(TAG_MERKLE, c0, c1, c2, c3)
//
// Rust port of `sdk/src/crypto/merkle.ts`. MUST stay byte-identical to the
// SDK and to `circuits/src/lib/merkle.circom`. Used by:
//   - fmd-webserver: serve `/v1/path/:cm` and `/v1/tree-state`.
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
    leaves: Vec<Field>,
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
            leaves: Vec::new(),
            zeros,
        })
    }

    pub fn insert(&mut self, leaf: Field) -> usize {
        self.leaves.push(leaf);
        self.leaves.len() - 1
    }

    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    /// Drop the last `n` leaves. Used by callers that speculatively
    /// `insert` and need to undo on failure (relayer rollback).
    pub fn truncate_leaves(&mut self, n: usize) {
        let len = self.leaves.len();
        let keep = len.saturating_sub(n);
        self.leaves.truncate(keep);
    }

    pub fn root(&self) -> Result<Field, TreeError> {
        self.node_at(self.depth, 0)
    }

    /// Bottom-up parallel root rebuild. Produces byte-identical output to
    /// `root()` (same Poseidon math, same zero padding) but hashes each level
    /// across threads via rayon. Use for bulk reconstructions (bootstrap
    /// replay); per-insert callers should stick with `root()`.
    pub fn root_par(&self) -> Result<Field, TreeError> {
        if self.leaves.is_empty() {
            return Ok(self.zeros[self.depth]);
        }
        let mut cur: Vec<Field> = self.leaves.clone();
        for lvl in 0..self.depth {
            let zero = self.zeros[lvl];
            let rem = cur.len() % ARITY;
            if rem != 0 {
                cur.extend(std::iter::repeat_n(zero, ARITY - rem));
            }
            cur = cur
                .par_chunks(ARITY)
                .map(|c| hash_node(&c[0], &c[1], &c[2], &c[3]))
                .collect::<Result<Vec<Field>, TreeError>>()?;
        }
        debug_assert_eq!(cur.len(), 1);
        Ok(cur[0])
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
        let n = self.leaves.len();
        let mut out: Vec<[Field; 3]> = Vec::with_capacity(self.depth);
        for lvl in 0..self.depth {
            let stride = ARITY.pow(lvl as u32);
            let slot = (n / stride) % ARITY;
            let parent_idx = n / (stride * ARITY);
            let mut row = [[0u8; 32]; 3];
            #[allow(clippy::needless_range_loop)]
            for k in 0..3 {
                if k < slot {
                    row[k] = self.node_at(lvl, parent_idx * ARITY + k)?;
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

    pub fn proof(&self, leaf_index: usize) -> Result<MerkleProof, TreeError> {
        if leaf_index >= self.leaves.len() && !self.leaves.is_empty() {
            // Not-yet-inserted leaves get zero siblings via node_at's zero
            // fallback, matching SDK behaviour.
        }
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
                    sibs[s] = self.node_at(level, parent_idx * ARITY + k)?;
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

    fn node_at(&self, level: usize, index: usize) -> Result<Field, TreeError> {
        if level == 0 {
            return Ok(self.leaves.get(index).copied().unwrap_or([0u8; 32]));
        }
        let subtree_start = index * ARITY.pow(level as u32);
        if subtree_start >= self.leaves.len() {
            return Ok(self.zeros[level]);
        }
        let child_level = level - 1;
        let first_child = index * ARITY;
        let c0 = self.node_at(child_level, first_child)?;
        let c1 = self.node_at(child_level, first_child + 1)?;
        let c2 = self.node_at(child_level, first_child + 2)?;
        let c3 = self.node_at(child_level, first_child + 3)?;
        hash_node(&c0, &c1, &c2, &c3)
    }
}

#[cfg(test)]
mod tests;
