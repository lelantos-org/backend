//! Frontier-only incremental view of the same quaternary tree as [`MerkleTree`].
//!
//! [`MerkleTree`] materialises every node, costing about 1.33 leaves' worth of
//! memory. A writer that only ever appends and only ever reads the root and the
//! frontier does not need them: the frontier *is* an append-only tree's complete
//! resume state, which is why the contract keeps `filledSubtrees` and nothing
//! else. This type carries `depth × 3` field elements — about a kilobyte at
//! `DEPTH` — and pays `depth` Poseidon calls per leaf.
//!
//! Its state is byte-identical to [`MerkleTree::frontier`] at the same leaf
//! count, and `tests` holds the differential proof of that. Anything needing
//! sibling paths, `truncate_leaves`, or random access still wants the full tree.
//!
//! [`MerkleTree`]: super::MerkleTree
//! [`MerkleTree::frontier`]: super::MerkleTree::frontier

use super::hash::{ARITY, hash_node};
use super::{Field, PAR_THRESHOLD, TreeError, zero_levels};
use rayon::prelude::*;

/// Root, frontier and leaf count of an append-only quaternary tree.
///
/// `Clone` is how a writer that appends speculatively undoes itself: the whole
/// state is `depth × 3` field elements, so keeping a copy from before a batch
/// costs about a kilobyte and restoring it is a move. A frontier cannot drop
/// leaves the way [`MerkleTree::truncate_leaves`] can -- it has no record of
/// what it folded -- so the copy is the rollback.
///
/// [`MerkleTree::truncate_leaves`]: super::MerkleTree::truncate_leaves
#[derive(Clone)]
pub struct Frontier {
    depth: usize,
    leaf_count: u64,
    /// `depth × 3` working slots. Row `lvl` position `k` holds the node that
    /// will sit at child `k` of the parent the next insert lands under.
    ///
    /// Positions at or past the current insert slot hold *provisional* values:
    /// a partial subtree that later leaves will keep overwriting. They are never
    /// read while provisional, because [`Self::fold`] reads only `k < slot`, and
    /// each is complete by the time the slot advances past it. [`Self::slots`]
    /// masks them off so what is persisted matches
    /// [`MerkleTree::frontier`] exactly.
    ///
    /// [`MerkleTree::frontier`]: super::MerkleTree::frontier
    slots: Vec<[Field; 3]>,
    zeros: Vec<Field>,
    root: Field,
}

impl Frontier {
    /// Empty tree of `depth`.
    pub fn new(depth: usize) -> Result<Self, TreeError> {
        let zeros = zero_levels(depth)?;
        let root = zeros[depth];
        Ok(Self {
            depth,
            leaf_count: 0,
            slots: vec![[[0u8; 32]; 3]; depth],
            zeros,
            root,
        })
    }

    /// Rebuild from persisted state: `leaf_count`, `slots` and the root that was
    /// stored alongside them.
    ///
    /// The stored root is checked rather than trusted. It is a fold over `slots`
    /// and `leaf_count`, so a root that disagrees with them means the two were
    /// written from different states, and folding onto that would put a root on
    /// the wire the chain never held. Refusing costs `depth` hashes once per
    /// process and turns a silently wrong mirror into a boot failure.
    ///
    /// The one leaf count where the fold cannot stand in for the stored root is
    /// an exactly full tree. `slots` holds a level's *left* siblings, and at
    /// capacity every level's slot is 0, so a full tree and an empty one store
    /// the same all-zero frontier; the leaves that distinguish them were folded
    /// away. `leaf_count` still tells the two apart, which is why the stored root
    /// is taken as given there and checked everywhere else.
    pub fn resume(
        depth: usize,
        leaf_count: u64,
        slots: Vec<[Field; 3]>,
        root: Field,
    ) -> Result<Self, TreeError> {
        if slots.len() != depth {
            return Err(TreeError::BadFieldLength(slots.len() * 3 * 32));
        }
        let f = Self {
            depth,
            leaf_count,
            slots,
            zeros: zero_levels(depth)?,
            root,
        };
        f.capacity_check(leaf_count)?;
        if leaf_count < f.capacity()? {
            let folded = f.fold_root()?;
            if folded != root {
                return Err(TreeError::RootMismatch {
                    stored: hex::encode(root),
                    folded: hex::encode(folded),
                });
            }
        }
        Ok(f)
    }

    /// Append one leaf. `depth` hashes, no allocation beyond the batch of one.
    pub fn push(&mut self, leaf: Field) -> Result<(), TreeError> {
        self.extend([leaf])
    }

    /// Append many leaves for the cost of one root.
    ///
    /// [`Self::push`] hashes `depth` times per leaf, because it recomputes the
    /// root the leaf just moved. A batch only has to publish the root once, so
    /// this instead carries each level's *completed* groups up and folds the
    /// root at the end: `N / 3` hashes for the cascade -- a group of four costs
    /// one hash, and only every fourth group reaches the next level -- plus
    /// `depth` for the final fold, against `N * depth` for a `push` loop. At
    /// [`DEPTH`] that is roughly a thirtyfold drop in Poseidon calls over a
    /// bootstrap replay, and it is the same arithmetic either way: `tests` holds
    /// the differential proof against a `push` loop and against
    /// [`MerkleTree::frontier`].
    ///
    /// Unlike [`MerkleTree::extend`] this holds only the level being folded, so
    /// replaying a full tree costs a page of leaves rather than 1.33 times the
    /// history.
    ///
    /// [`DEPTH`]: super::DEPTH
    /// [`MerkleTree::extend`]: super::MerkleTree::extend
    /// [`MerkleTree::frontier`]: super::MerkleTree::frontier
    pub fn extend(&mut self, leaves: impl IntoIterator<Item = Field>) -> Result<(), TreeError> {
        let mut nodes: Vec<Field> = leaves.into_iter().collect();
        if nodes.is_empty() {
            return Ok(());
        }
        let added = nodes.len() as u64;
        self.capacity_check(self.leaf_count + added)?;

        // Root of an exactly-full tree, which no fold over `slots` can recover:
        // at capacity every level's slot is 0, so the fold reads no left
        // siblings and returns the empty root. The cascade is the one place that
        // still holds the top group, so it keeps the value on the way past.
        let mut full_root: Option<Field> = None;
        // Index, at the level being folded, of the first node in `nodes`.
        let mut idx = self.leaf_count;

        for lvl in 0..self.depth {
            if nodes.is_empty() {
                break;
            }
            let zero = self.zeros[lvl];
            let slot = (idx % ARITY as u64) as usize;
            // The level's group run starts at the `slot` left siblings already
            // standing in `slots`, so those and `nodes` together are the
            // children to divide into groups of `ARITY`.
            let head = self.slots[lvl];
            let total = slot + nodes.len();
            let complete = total / ARITY;
            let leftover = total % ARITY;
            let child = |i: usize| if i < slot { head[i] } else { nodes[i - slot] };

            let group = |g: usize| {
                let mut c = [zero; ARITY];
                for (k, cell) in c.iter_mut().enumerate() {
                    *cell = child(g * ARITY + k);
                }
                c
            };
            let parents: Vec<Field> = if complete < PAR_THRESHOLD {
                (0..complete)
                    .map(|g| hash_node(&group(g)))
                    .collect::<Result<_, _>>()?
            } else {
                (0..complete)
                    .into_par_iter()
                    .map(|g| hash_node(&group(g)))
                    .collect::<Result<_, _>>()?
            };

            // Whatever did not fill a group becomes the level's new left
            // siblings. Written whole, so positions past the new slot are zero
            // rather than a stale child of the group that just closed.
            let mut next_slots = [[0u8; 32]; ARITY - 1];
            for (k, cell) in next_slots.iter_mut().enumerate().take(leftover) {
                *cell = child(complete * ARITY + k);
            }
            self.slots[lvl] = next_slots;

            if lvl + 1 == self.depth && complete == 1 {
                full_root = Some(parents[0]);
            }
            nodes = parents;
            idx /= ARITY as u64;
        }

        self.leaf_count += added;
        self.root = match full_root {
            Some(root) => root,
            None => self.fold_root()?,
        };
        Ok(())
    }

    pub fn root(&self) -> Field {
        self.root
    }

    pub fn leaf_count(&self) -> u64 {
        self.leaf_count
    }

    /// The frontier to persist: [`Self::slots`] with the provisional positions
    /// masked to zero, which is byte-for-byte what `MerkleTree::frontier`
    /// returns at this leaf count.
    ///
    /// Masked rather than raw because the mask is what makes the value a
    /// function of the tree alone. Two frontiers at the same leaf count then
    /// compare equal regardless of how they got there, which is what lets a
    /// stored row be checked against the chain's published root.
    pub fn slots(&self) -> Vec<[Field; 3]> {
        (0..self.depth)
            .map(|lvl| {
                let slot = self.slot_at(lvl);
                let mut row = [[0u8; 32]; 3];
                row[..slot].copy_from_slice(&self.slots[lvl][..slot]);
                row
            })
            .collect()
    }

    /// Root implied by `slots` and `leaf_count`: `depth` hashes.
    ///
    /// The next leaf's position is empty, so the fold starts from the level-0
    /// zero and carries each level's node up through its left siblings.
    fn fold_root(&self) -> Result<Field, TreeError> {
        let mut cur = self.zeros[0];
        let mut idx = self.leaf_count;
        for lvl in 0..self.depth {
            cur = self.fold(lvl, (idx % ARITY as u64) as usize, &cur)?;
            idx /= ARITY as u64;
        }
        Ok(cur)
    }

    /// Child position the next insert takes at `lvl`, and so the count of left
    /// siblings already filled there.
    fn slot_at(&self, lvl: usize) -> usize {
        ((self.leaf_count / (ARITY as u64).pow(lvl as u32)) % ARITY as u64) as usize
    }

    /// Hash level `lvl`'s parent group: left siblings from `slots`, `cur` at
    /// `slot`, and the level's zero subtree to the right.
    fn fold(&self, lvl: usize, slot: usize, cur: &Field) -> Result<Field, TreeError> {
        let mut c = [self.zeros[lvl]; ARITY];
        c[..slot].copy_from_slice(&self.slots[lvl][..slot]);
        c[slot] = *cur;
        hash_node(&c)
    }

    /// Leaves the tree holds: `ARITY^depth`. Fallible only because a depth past
    /// 31 has no `u64` capacity to speak of.
    fn capacity(&self) -> Result<u64, TreeError> {
        (ARITY as u64)
            .checked_pow(self.depth as u32)
            .ok_or(TreeError::OutOfRange(self.depth))
    }

    /// `ARITY^depth` leaves fit. Rejected rather than wrapped: past capacity the
    /// index folds back onto leaf 0 and the root silently stops matching chain.
    fn capacity_check(&self, leaves: u64) -> Result<(), TreeError> {
        if leaves > self.capacity()? {
            return Err(TreeError::OutOfRange(leaves as usize));
        }
        Ok(())
    }
}

/// Wire and storage layout for a frontier: `depth × 3` big-endian field elements
/// concatenated, so `depth * 96` bytes.
///
/// Here rather than in each service because the writer and the reader are
/// different processes. A disagreement about the layout produces a well-formed
/// root for a tree that never existed, which no caller can detect.
pub fn encode_frontier(slots: &[[Field; 3]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(slots.len() * 3 * 32);
    for row in slots {
        for field in row {
            out.extend_from_slice(field);
        }
    }
    out
}

/// Inverse of [`encode_frontier`]. Rejects any length but `depth * 96`.
pub fn decode_frontier(depth: usize, bytes: &[u8]) -> Result<Vec<[Field; 3]>, TreeError> {
    if bytes.len() != depth * 3 * 32 {
        return Err(TreeError::BadFieldLength(bytes.len()));
    }
    Ok(bytes
        .chunks_exact(3 * 32)
        .map(|row| {
            let mut out = [[0u8; 32]; 3];
            for (slot, src) in out.iter_mut().zip(row.chunks_exact(32)) {
                slot.copy_from_slice(src);
            }
            out
        })
        .collect())
}
