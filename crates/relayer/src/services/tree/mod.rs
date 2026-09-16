//! Per-chain in-memory tree mirror, resumed from fmd-indexer's `tree_state` row
//! at startup. The relayer is otherwise stateless across restarts and owns no
//! tables.
//!
//! The mirror is a [`Frontier`] rather than a materialised
//! [`MerkleTree`](crypto::tree::MerkleTree): it
//! only ever appends, and only ever reads the root and the frontier, which is
//! exactly an append-only tree's resume state. That is what lets a boot read one
//! kilobyte instead of replaying every `notes` row, and it is why the contract
//! stores `filledSubtrees` and nothing else.
//!
//! Each chain owns one `Arc<Mutex<TreeMirror>>`. The chain's batcher holds the
//! mutex through reserve, prove, submit and receipt. A bundle reserves several
//! items in a row, each building on the one before; see
//! [`TreeMirror::begin_bundle`]. What the chain did not keep is unwound.

mod bootstrap;
mod bundle;
mod leaf;
mod snapshot;

pub use snapshot::MirrorSnapshot;

use crate::domain::error::{AppError, AppResult};
use alloy::primitives::U256;
use bundle::{Bundle, BundleEntry};
use crypto::tree::{Field, Frontier};
use leaf::leaf_hash;
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::error;

/// Merkle depth this mirror is built for.
///
/// Re-exported rather than declared: the depth is pinned by the circuits and
/// the verifier, so every service that mirrors the tree has to agree on one
/// value, and `crypto::tree` is the crate they all share.
pub use crypto::tree::DEPTH;
/// Quaternary tree, so `ARITY^DEPTH` leaves. Mirrors `MASP.MAX_LEAVES`.
const MAX_LEAVES: usize = 4usize.pow(DEPTH as u32);

pub struct TreeMirror {
    pub chain_id: i64,
    tree: Frontier,
    /// The tree as it stood before the batch currently in flight, and the only
    /// way back: a frontier keeps no record of what it folded, so it cannot drop
    /// leaves the way a materialised tree can. Taken at every reserve, restored
    /// by [`TreeMirror::rollback`], and about a kilobyte either way.
    checkpoint: Frontier,
    /// Why this mirror was parked, if it was; see [`TreeMirror::unwind`]. Every
    /// reserve then fails fast rather than building on state that may not match
    /// the chain.
    desynced: Option<String>,
    /// Lock-free copy of what `/chains` reports, refreshed on every mutation.
    ///
    /// The mirror mutex is held from reserve through prove and submit, tens of
    /// seconds, and `/chains` is what every wallet calls at boot. Reading through
    /// the mutex would queue that endpoint behind whatever spend is in flight, so
    /// the readings are published here.
    snapshot: Arc<MirrorSnapshot>,
    /// Roots this mirror has held, newest last, bounded to [`ROOT_HISTORY`].
    ///
    /// The pool accepts a proof against any root in its own recent window, so a
    /// payload naming an older one is valid. A root the relayer has never held is
    /// not: that proof cannot land, and catching it here saves a Groth16 and a
    /// revert.
    ///
    /// Exactly the pool's ring in order once bootstrapped, so an entry's age also
    /// locates it in the ring; see [`Self::anchor_index`].
    recent_roots: VecDeque<Field>,
    /// Ring slot of the newest `recent_roots` entry in the pool's `roots` buffer,
    /// that is `CommitmentTree.rootIndex`. Moves with the window: one slot per
    /// remembered root, back one per retracted root.
    ///
    /// `None` between [`Self::bootstrap`] and [`Self::verify_chain_root`], which
    /// reads it from the pool; no spend can be encoded without it.
    ring_index: Option<usize>,
    /// The bundle being reserved, if one is open; see [`Self::begin_bundle`].
    bundle: Option<Bundle>,
}

/// How many past roots a payload may name. Matches the pool's own accepted
/// window, `CommitmentTree.ROOT_HISTORY`; a spend proved against anything older
/// cannot land anyway.
pub const ROOT_HISTORY: usize = 64;

/// The tree position a submission has claimed, plus the state it must prove
/// the advance from.
#[derive(Debug)]
pub struct ReservedSlot {
    pub start_index: u64,
    pub old_root: Field,
    pub old_frontier: Vec<[Field; 3]>,
    /// Ring slot of the root the item's spend proved membership against, its
    /// `SpendTree.anchorIndex`. Set by the batcher for items that name one; see
    /// [`TreeMirror::anchor_index`].
    pub anchor_index: Option<u8>,
}

/// The state that claim advances the tree to.
#[derive(Debug)]
pub struct AdvancedState {
    pub new_root: Field,
}

impl TreeMirror {
    pub fn new(chain_id: i64) -> AppResult<Self> {
        let tree = Frontier::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
        let mut m = Self {
            chain_id,
            checkpoint: tree.clone(),
            tree,
            desynced: None,
            snapshot: Arc::new(MirrorSnapshot::default()),
            recent_roots: VecDeque::with_capacity(ROOT_HISTORY),
            // A fresh pool: the empty root `publish` remembers below sits at slot
            // 0, and remembering it does not move the ring.
            ring_index: None,
            bundle: None,
        };
        m.publish();
        m.ring_index = Some(0);
        Ok(m)
    }

    /// Handle `/chains` reads from, without taking the mirror lock.
    pub fn snapshot(&self) -> Arc<MirrorSnapshot> {
        self.snapshot.clone()
    }

    /// Whether `root` is one this mirror has held recently.
    ///
    /// Unknown roots are the common cause of an `UnknownRoot` revert, and a caller
    /// told so can act on it, unlike the opaque 502 the revert produces.
    pub fn knows_root(&self, root: &Field) -> bool {
        self.recent_roots.contains(root)
    }

    /// How many advances ago `root` was current: 0 for the current root, `None`
    /// for one outside the window.
    ///
    /// The pool evicts one root per advance, so a root of age `a` survives `k` more
    /// advances only while `a + k < ROOT_HISTORY`. A bundle of `k` items must
    /// check that for every payload in it.
    pub fn root_age(&self, root: &Field) -> Option<usize> {
        self.recent_roots.iter().rev().position(|r| r == root)
    }

    /// The pool's ring slot holding `root`: the `SpendTree.anchorIndex` a spend
    /// proved against it passes, as `MASP.rootIndexOf` would answer.
    ///
    /// The window is the ring in order, so a root of age `a` sits `a` slots behind
    /// the newest. A slot never moves once written, only gets overwritten
    /// [`ROOT_HISTORY`] advances later, so the answer is the same before and after
    /// any reservation that does not evict the root; the batcher's
    /// `drop_stale_roots` rules eviction out for a whole bundle. A root held twice
    /// resolves to its newest slot, as the pool's own scan does.
    pub fn anchor_index(&self, root: &Field) -> AppResult<u8> {
        let age = self.root_age(root).ok_or_else(|| {
            AppError::BadRequest(
                "pubInputs.merkleRoot is not a root this relayer has held recently; \
                 refresh the tree state and re-prove"
                    .into(),
            )
        })?;
        let newest = self.ring_index.ok_or_else(|| {
            AppError::Internal(format!(
                "chain {}: the pool's root ring position has not been read",
                self.chain_id
            ))
        })?;
        // `age < ROOT_HISTORY`, as the window never holds more.
        Ok(((newest + ROOT_HISTORY - age) % ROOT_HISTORY) as u8)
    }

    /// Refresh the published readings and the accepted-root window. Called
    /// after every mutation.
    fn publish(&mut self) {
        let root = self.tree.root();
        self.snapshot
            .publish(self.tree.leaf_count(), root, self.desynced.is_some());
        self.remember_root(root);
    }

    /// Drop `root` from the accepted window, if it is still the newest entry.
    ///
    /// The inverse of [`Self::remember_root`], for an advance being undone. Only
    /// the newest entry is eligible: an identical root deeper in the window was
    /// reached by a path that landed and is still valid.
    fn forget_newest_root(&mut self, root: &Field) {
        if self.recent_roots.back() == Some(root) {
            self.recent_roots.pop_back();
            self.ring_index = self
                .ring_index
                .map(|i| (i + ROOT_HISTORY - 1) % ROOT_HISTORY);
        }
    }

    /// Append `root` to the accepted window, newest last, dropping the oldest
    /// once it is full. A repeat of the newest entry is a no-op, so a mutation
    /// that leaves the root unchanged does not consume a slot.
    ///
    /// Every new root is one advance on chain, which writes the next ring slot, so
    /// the ring position moves with it.
    fn remember_root(&mut self, root: Field) {
        if self.recent_roots.back() == Some(&root) {
            return;
        }
        if self.recent_roots.len() == ROOT_HISTORY {
            self.recent_roots.pop_front();
        }
        self.recent_roots.push_back(root);
        self.ring_index = self.ring_index.map(|i| (i + 1) % ROOT_HISTORY);
    }

    pub fn is_desynced(&self) -> bool {
        self.desynced.is_some()
    }

    fn check_usable(&self) -> AppResult<()> {
        match &self.desynced {
            None => Ok(()),
            Some(reason) => Err(AppError::MirrorDesynced(format!(
                "chain {}: {} (the batcher resyncs once the indexer has caught up)",
                self.chain_id, reason
            ))),
        }
    }

    pub fn committed_count(&self) -> u64 {
        self.tree.leaf_count()
    }

    /// Infallible: a frontier carries its root rather than folding one on
    /// demand, so there is no failure for a caller to handle.
    pub fn current_root(&self) -> Field {
        self.tree.root()
    }

    /// Insert `(cm, cv_dep)` pairs. The mirror hashes each pair into a leaf before
    /// insertion to stay in sync with the on-chain tree, which advances through
    /// SNARK-verified leaf roots.
    pub fn reserve_and_advance_batch(
        &mut self,
        cms: &[(Field, [U256; 2])],
    ) -> AppResult<(ReservedSlot, AdvancedState)> {
        self.check_usable()?;
        let start_index = self.tree.leaf_count();

        // Capacity first: a length check, so an oversized batch is refused without
        // computing a single Poseidon. Widened to `u64` rather than narrowing the
        // leaf count to `usize`, so the comparison cannot truncate.
        if start_index + cms.len() as u64 > MAX_LEAVES as u64 {
            return Err(AppError::BadRequest(format!(
                "chain {}: tree is full ({} leaves, {} more requested, capacity {})",
                self.chain_id,
                start_index,
                cms.len(),
                MAX_LEAVES
            )));
        }

        // Then hash every leaf up front. `leaf_hash` is Poseidon, which rejects a
        // non-canonical input, and `cm` and `cv_dep` are wallet-supplied on the
        // spend and swap paths. Hashing inside the insert loop would fail after
        // earlier leaves had gone in, leaving the mirror one leaf ahead of the
        // chain with no rollback and no park. Nothing mutates until every leaf is
        // known good.
        let leaves = cms
            .iter()
            .map(|(cm, cv_dep)| leaf_hash(cm, cv_dep))
            .collect::<AppResult<Vec<Field>>>()?;

        let old_root = self.tree.root();
        let old_frontier = self.tree.slots();

        // The state to return to if this batch does not land. Taken before the
        // first push, since that is the last moment the mirror still matches the
        // chain.
        self.checkpoint = self.tree.clone();

        // Past this point the tree is mutated, so any failure must be unwound
        // rather than propagated directly; see `insert_all`.
        let inserted = self.insert_all(leaves)?;
        debug_assert_eq!(inserted, cms.len());

        let new_root = self.tree.root();
        if let Some(bundle) = self.bundle.as_mut() {
            bundle.items.push(BundleEntry {
                before: self.checkpoint.clone(),
                new_root,
            });
        }
        self.publish();
        Ok((
            ReservedSlot {
                start_index,
                old_root,
                old_frontier,
                anchor_index: None,
            },
            AdvancedState { new_root },
        ))
    }

    /// Insert pre-hashed leaves, leaving the tree untouched if any insert fails.
    /// `Frontier::push` should not fail once capacity is checked, but a partial
    /// batch is the state that desyncs a mirror permanently, so it is undone here
    /// and the mirror parked if that also fails.
    fn insert_all(&mut self, leaves: Vec<Field>) -> AppResult<usize> {
        let n = leaves.len();
        for (i, leaf) in leaves.into_iter().enumerate() {
            if let Err(e) = self.tree.push(leaf) {
                let cause = AppError::Internal(format!(
                    "chain {}: leaf {} of {} failed to insert: {}",
                    self.chain_id, i, n, e
                ));
                error!(chain_id = self.chain_id, error = %cause, "partial batch insert; undoing");
                if let Err(rollback_err) = self.rollback(i) {
                    self.park(format!("partial batch insert: {rollback_err}"));
                }
                return Err(cause);
            }
        }
        Ok(n)
    }
}

/// The empty tree's root, `CommitmentTree.EMPTY_ROOT`: what genesis seeds ring
/// slot 0 with.
fn empty_root() -> AppResult<Field> {
    Ok(Frontier::new(DEPTH)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .root())
}

pub fn vec_to_field(v: &[u8]) -> AppResult<Field> {
    crypto::tree::field_from_bytes(v).map_err(|e| AppError::Internal(e.to_string()))
}

pub fn field_to_hex(f: &Field) -> String {
    format!("0x{}", hex::encode(f))
}

#[cfg(test)]
mod tests;
