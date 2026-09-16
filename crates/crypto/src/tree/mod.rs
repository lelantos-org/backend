//! Quaternary sparse Merkle tree with Poseidon-arity-5 nodes:
//! `node = Poseidon(TAG_MERKLE, c0, c1, c2, c3)`.
//!
//! Rust port of `sdk/src/crypto/merkle.ts`. Must stay byte-identical to the SDK
//! and to `circuits/src/lib/merkle.circom`. Used by fmd-webserver to serve
//! `/v1/tree-state` and by the relayer to build tree_update witnesses (frontier
//! and path indices).
//!
//! The tree is public data rather than a privacy-sensitive primitive, but lives
//! in `crypto` because it shares the Poseidon dependency and is consumed only
//! by FMD-zone crates.

mod frontier;
mod hash;

use hash::{ARITY, hash_node};
use rayon::prelude::*;
use std::sync::OnceLock;
use thiserror::Error;

pub use frontier::{Frontier, decode_frontier, encode_frontier};
pub use hash::{TAG_LEAF, TAG_MERKLE, leaf_hash};
/// Field elements cross this crate's boundary big-endian. This is the single
/// conversion pair, shared with `note` so the two cannot disagree.
pub(crate) use hash::{be_to_fq, fq_to_be};

#[derive(Debug, Error)]
pub enum TreeError {
    #[error("poseidon: {0}")]
    Poseidon(String),
    #[error("leaf index {0} out of range")]
    OutOfRange(usize),
    #[error("expected 32-byte field, got {0}")]
    BadFieldLength(usize),
    #[error("stored root {stored} is not the fold of the stored frontier, which gives {folded}")]
    RootMismatch { stored: String, folded: String },
}

/// Merkle depth of the deployed commitment tree.
///
/// A constant rather than configuration, because it is pinned at compile time
/// in three places that must all agree and none of which a deployment can
/// vary: `circuits/src/lib/common.circom` asserts `d <= 11` (its empty-subtree
/// table stops there), `MASP.MAX_LEAVES` is `4^11`, and the verifying key is
/// built for that geometry. Changing the depth means new circuits and a new
/// verifier, not a new environment variable.
///
/// Lives here because this crate owns `MerkleTree`, and it is the one crate
/// every service that mirrors the tree already depends on. A service that
/// picked its own value would serve a well-formed root for a tree the chain
/// never held, which no caller can detect and every wallet then rejects its
/// own correct tree against.
pub const DEPTH: usize = 11;

/// Nodes per level below which hashing stays on the calling thread.
///
/// A Poseidon-5 node is a few microseconds, so a level of one or two nodes is
/// swamped by rayon's split and join. Sized so the parallel path only opens for
/// work that can actually fill a core.
const PAR_THRESHOLD: usize = 16;

/// Big-endian 32-byte field element (matches SDK `Field`).
pub type Field = [u8; 32];

/// Read a field element out of a database column or a wire value.
pub fn field_from_bytes(bytes: &[u8]) -> Result<Field, TreeError> {
    bytes
        .try_into()
        .map_err(|_| TreeError::BadFieldLength(bytes.len()))
}

/// `zeros[d]` is the root of an all-zero subtree of height `d`, so `zeros[0]` is
/// the empty leaf and `zeros[depth]` the empty root.
///
/// Shared by [`MerkleTree`] and [`Frontier`]: the two must agree on what an
/// absent node hashes to, or their roots diverge on any tree that is not exactly
/// full.
/// Memoised table for [`DEPTH`], the only depth any service runs at.
///
/// Building the table costs `depth` Poseidon hashes, and the webserver builds a
/// [`Frontier`] per request to answer `/v1/tree-state` for an empty chain. The
/// table is a pure function of the depth, so one process-wide copy serves every
/// caller; the clone that hands it out is under half a kilobyte.
static DEPTH_ZEROS: OnceLock<Vec<Field>> = OnceLock::new();

fn zero_levels(depth: usize) -> Result<Vec<Field>, TreeError> {
    if depth == DEPTH {
        // `get_or_init` cannot run a fallible initialiser, so compute first and
        // let the race loser drop its copy.
        if let Some(cached) = DEPTH_ZEROS.get() {
            return Ok(cached.clone());
        }
        let built = compute_zero_levels(depth)?;
        return Ok(DEPTH_ZEROS.get_or_init(|| built).clone());
    }
    compute_zero_levels(depth)
}

fn compute_zero_levels(depth: usize) -> Result<Vec<Field>, TreeError> {
    let mut zeros: Vec<Field> = Vec::with_capacity(depth + 1);
    let mut z: Field = [0u8; 32];
    for _ in 0..depth {
        zeros.push(z);
        z = hash_node(&[z; ARITY])?;
    }
    zeros.push(z);
    Ok(zeros)
}

#[derive(Debug, Clone)]
pub struct MerkleProof {
    pub path_elements: Vec<[Field; 3]>,
    pub path_indices: Vec<u8>,
}

pub struct MerkleTree {
    pub depth: usize,
    /// Materialised nodes per level: `levels[0]` holds the leaves and
    /// `levels[d][i]` the node at level `d`, index `i`. Indices past a level's
    /// length are implicitly `zeros[d]`; since `zeros[d+1] = hash(zeros[d] × 4)`,
    /// an absent node and a materialised all-zero subtree hold the same value.
    ///
    /// Kept in sync by every mutation, so `root()` is O(1) and `frontier()` and
    /// `proof()` are O(depth) table lookups, at a cost of about 1.33 times the
    /// leaf count in memory.
    levels: Vec<Vec<Field>>,
    zeros: Vec<Field>,
}

impl MerkleTree {
    pub fn new(depth: usize) -> Result<Self, TreeError> {
        Ok(Self {
            depth,
            levels: vec![Vec::new(); depth + 1],
            zeros: zero_levels(depth)?,
        })
    }

    /// Append one leaf and refresh the `depth` nodes on its path to the root.
    pub fn insert(&mut self, leaf: Field) -> Result<usize, TreeError> {
        self.levels[0].push(leaf);
        let index = self.levels[0].len() - 1;
        self.refresh_path(index)?;
        Ok(index)
    }

    /// Append many leaves and rebuild the internal levels bottom-up in parallel.
    /// O(N) in total rather than the O(N · depth) of N `insert` calls, so this is
    /// the path for bootstrap replay.
    ///
    /// Only the nodes above the appended leaves are recomputed, so replaying a
    /// history in pages costs O(N) over the whole replay rather than O(N) per
    /// page.
    pub fn extend(&mut self, leaves: impl IntoIterator<Item = Field>) -> Result<(), TreeError> {
        let dirty_from = self.levels[0].len();
        self.levels[0].extend(leaves);
        self.rebuild_from(dirty_from)
    }

    pub fn leaf_count(&self) -> usize {
        self.levels[0].len()
    }

    /// Drop the last `n` leaves, for callers that insert speculatively and undo
    /// on failure, such as relayer rollback.
    ///
    /// Every level shrinks to `ceil(child_len / ARITY)`, and only its new last
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
                self.levels[lvl + 1][parent] = hash_node(&self.group_at(lvl, first))?;
            }
        }
        Ok(())
    }

    pub fn root(&self) -> Result<Field, TreeError> {
        Ok(self.node_at(self.depth, 0))
    }

    /// Re-hash the path from `leaf_index` to the root. Each level's parent index
    /// grows by at most one slot, so the `resize` appends at most one entry and
    /// never leaves a gap.
    fn refresh_path(&mut self, leaf_index: usize) -> Result<(), TreeError> {
        let mut idx = leaf_index;
        for lvl in 0..self.depth {
            let parent = idx / ARITY;
            let first = parent * ARITY;
            let h = hash_node(&self.group_at(lvl, first))?;
            let up = lvl + 1;
            if self.levels[up].len() <= parent {
                self.levels[up].resize(parent + 1, self.zeros[up]);
            }
            self.levels[up][parent] = h;
            idx = parent;
        }
        Ok(())
    }

    /// Rebuild the internal levels above leaf `first_dirty`, hashing each
    /// level's affected nodes across threads via rayon.
    ///
    /// Every parent of an untouched leaf is itself untouched, so each level only
    /// has to recompute from `first_dirty / ARITY` upwards; the dirty index
    /// divides down as the walk climbs. Levels below the dirty index are already
    /// correct from the mutation that filled them.
    fn rebuild_from(&mut self, first_dirty: usize) -> Result<(), TreeError> {
        let mut dirty = first_dirty;
        for lvl in 0..self.depth {
            let zero = self.zeros[lvl];
            // Floor, not ceil: the parent that `dirty` falls under is itself
            // dirty, since it gained a child.
            let first_parent = dirty / ARITY;
            let parents = {
                let child = &self.levels[lvl];
                let parent_len = child.len().div_ceil(ARITY);
                let group = |p: usize| {
                    let f = p * ARITY;
                    let mut c = [zero; ARITY];
                    for (k, slot) in c.iter_mut().enumerate() {
                        if let Some(v) = child.get(f + k) {
                            *slot = *v;
                        }
                    }
                    c
                };
                let range = first_parent..parent_len;
                // Rayon costs more to split and join than a handful of hashes
                // takes, and the steady-state caller appends a block's worth of
                // leaves, not a history's. The threshold keeps the parallel path
                // for bootstrap replay and off the per-block path.
                if range.len() < PAR_THRESHOLD {
                    range
                        .map(|p| hash_node(&group(p)))
                        .collect::<Result<Vec<Field>, TreeError>>()?
                } else {
                    range
                        .into_par_iter()
                        .map(|p| hash_node(&group(p)))
                        .collect::<Result<Vec<Field>, TreeError>>()?
                }
            };
            // `resize` rather than `truncate`: it leaves the level *exactly*
            // `first_parent` long either way, so the `extend` below lands at the
            // right offset even if the level were somehow short, where a truncate
            // would silently append past a gap and shift every node above it. The
            // padding value is the level's zero subtree, which is what `node_at`
            // already reads for an absent node.
            self.levels[lvl + 1].resize(first_parent, self.zeros[lvl + 1]);
            self.levels[lvl + 1].extend(parents);
            debug_assert_eq!(
                self.levels[lvl + 1].len(),
                self.levels[lvl].len().div_ceil(ARITY)
            );
            dirty = first_parent;
        }
        Ok(())
    }

    /// Frontier of `depth × 3` slots, mirroring the on-chain `filledSubtrees`
    /// layout. For each level `lvl` and slot `k`:
    ///
    /// ```text
    /// frontier[lvl][k] = node_at(lvl, parent_idx * 4 + k)   if k < current_slot
    /// frontier[lvl][k] = 0                                  otherwise
    /// ```
    ///
    /// where `current_slot = (N / 4^lvl) % 4`, `parent_idx = N / 4^(lvl+1)` and
    /// `N = leaf_count()`. Entries at `k >= current_slot` are not read by the
    /// next insert at this level and are zeroed deterministically.
    pub fn frontier(&self) -> Result<Vec<[Field; 3]>, TreeError> {
        // `idx` is the leaf count divided down to this level, so `idx % ARITY`
        // is the level's current slot and `idx / ARITY` its parent. Carried
        // rather than recomputed as `ARITY.pow(lvl)` per level.
        let mut idx = self.leaf_count();
        let mut out: Vec<[Field; 3]> = Vec::with_capacity(self.depth);
        for lvl in 0..self.depth {
            let slot = idx % ARITY;
            let parent_idx = idx / ARITY;
            let mut row = [[0u8; 32]; 3];
            for (k, cell) in row.iter_mut().enumerate().take(slot) {
                *cell = self.node_at(lvl, parent_idx * ARITY + k);
            }
            out.push(row);
            idx = parent_idx;
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

    /// Sibling path for `leaf_index`. Leaves not yet inserted get zero siblings
    /// through `node_at`'s zero fallback, matching the SDK.
    pub fn proof(&self, leaf_index: usize) -> Result<MerkleProof, TreeError> {
        let mut path_elements: Vec<[Field; 3]> = Vec::with_capacity(self.depth);
        let mut path_indices: Vec<u8> = Vec::with_capacity(self.depth);
        let mut idx = leaf_index;
        for level in 0..self.depth {
            let self_pos = idx % ARITY;
            let group = self.group_at(level, (idx / ARITY) * ARITY);
            let mut sibs = [[0u8; 32]; 3];
            for (dst, (_, node)) in sibs
                .iter_mut()
                .zip(group.iter().enumerate().filter(|(k, _)| *k != self_pos))
            {
                *dst = *node;
            }
            path_elements.push(sibs);
            path_indices.push(self_pos as u8);
            idx /= ARITY;
        }
        Ok(MerkleProof {
            path_elements,
            path_indices,
        })
    }

    /// The four children of one parent, `first` being the group's first child
    /// index. The single place the arity is unrolled, so a caller cannot read
    /// three siblings and a stale fourth.
    fn group_at(&self, level: usize, first: usize) -> [Field; ARITY] {
        let mut out = [[0u8; 32]; ARITY];
        for (k, slot) in out.iter_mut().enumerate() {
            *slot = self.node_at(level, first + k);
        }
        out
    }

    /// Materialised node, or the level's zero-subtree constant when the index is
    /// past what has been filled.
    fn node_at(&self, level: usize, index: usize) -> Field {
        self.levels[level]
            .get(index)
            .copied()
            .unwrap_or(self.zeros[level])
    }
}

#[cfg(test)]
mod tests;
