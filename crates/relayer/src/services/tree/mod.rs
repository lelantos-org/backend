//! Per-chain in-memory tree mirror, resumed from fmd-indexer's `tree_state` row
//! at startup. The relayer is otherwise stateless across restarts and owns no
//! tables.
//!
//! The mirror is a [`Frontier`] rather than a materialised [`MerkleTree`]: it
//! only ever appends, and only ever reads the root and the frontier, which is
//! exactly an append-only tree's resume state. That is what lets a boot read one
//! kilobyte instead of replaying every `notes` row, and it is why the contract
//! stores `filledSubtrees` and nothing else.
//!
//! Each chain owns one `Arc<Mutex<TreeMirror>>`. The pipeline holds the mutex
//! through reserve, prove, submit and receipt, so the next bundle builds
//! optimistically against the post-state. A revert unwinds the speculative
//! inserts.

use crate::adapters::abi::IMasp;
use crate::adapters::numeric::bigdecimal_to_u256;
use crate::adapters::rpc::RpcEndpoint;
use crate::domain::error::{AppError, AppResult};
use crate::repositories::{notes, tree_advances, tree_state};
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use common_crypto::poseidon as common_poseidon;
use common_crypto::tree::{Field, Frontier, decode_frontier};
use database::DbPool;
use database::models::TreeStateRow;
use rayon::prelude::*;
use std::collections::VecDeque;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{error, info};

/// Merkle depth this mirror is built for.
///
/// Re-exported rather than declared: the depth is pinned by the circuits and
/// the verifier, so every service that mirrors the tree has to agree on one
/// value, and `common_crypto::tree` is the crate they all share.
pub use common_crypto::tree::DEPTH;
/// Quaternary tree, so `ARITY^DEPTH` leaves. Mirrors `MASP.MAX_LEAVES`.
const MAX_LEAVES: usize = 4usize.pow(DEPTH as u32);
/// Leaves read per round trip during [`TreeMirror::bootstrap`].
const LEAF_PAGE: i64 = 100_000;
/// Domain-separation tag for Merkle leaf hashing, mirroring `TAG_LEAF` in
/// `circuits/src/lib/tags.circom`. `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x,
/// cv_dep_y)`, so a spender can rebuild the same leaf hash from `(cm, cv_dep)`
/// without learning anything else about the deposit.
const TAG_LEAF: u64 = 10;

/// Compute the in-circuit Merkle leaf:
/// `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
///
/// Must match `tree_update_batch.circom` byte for byte; drift here desyncs the
/// relayer's mirror from the on-chain tree.
fn leaf_hash(cm: &Field, cv_dep: &[U256; 2]) -> AppResult<Field> {
    let mut tag = [0u8; 32];
    tag[31] = TAG_LEAF as u8;
    let cv_x = cv_dep[0].to_be_bytes::<32>();
    let cv_y = cv_dep[1].to_be_bytes::<32>();
    common_poseidon::hash_bytes_be(&[&tag, cm, &cv_x, &cv_y])
        .map_err(|e| AppError::Internal(format!("leaf_hash: {}", e)))
}

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
    recent_roots: VecDeque<Field>,
}

/// How many past roots a payload may name. Matches the pool's own accepted
/// window; a spend proved against anything older cannot land anyway.
const ROOT_HISTORY: usize = 32;

/// Where a mirror's starting state came from.
///
/// Worth naming in the boot log and in a divergence error: the two paths fail
/// for different reasons. A stale `tree_state` row means the indexer is behind,
/// which time fixes; a replay that diverges means `notes` disagrees with the
/// chain, which it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootSource {
    /// fmd-indexer's stored frontier: one row, the normal path.
    TreeState,
    /// Folded from `notes`, for a chain the indexer has not written yet.
    Notes,
}

impl BootSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::TreeState => "tree_state",
            Self::Notes => "notes",
        }
    }
}

mod snapshot;

pub use snapshot::MirrorSnapshot;
/// The tree position a submission has claimed, plus the state it must prove
/// the advance from.
#[derive(Debug)]
pub struct ReservedSlot {
    pub start_index: u64,
    pub old_root: Field,
    pub old_frontier: Vec<[Field; 3]>,
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
        };
        m.publish();
        Ok(m)
    }

    /// Handle `/chains` reads from, without taking the mirror lock.
    pub fn snapshot(&self) -> Arc<MirrorSnapshot> {
        self.snapshot.clone()
    }

    /// Whether `root` is one this mirror has held recently.
    ///
    /// Unknown roots are the common cause of a `StaleOldRoot` revert, and a caller
    /// told so can act on it, unlike the opaque 502 the revert produces.
    pub fn knows_root(&self, root: &Field) -> bool {
        self.recent_roots.contains(root)
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
        }
    }

    /// Append `root` to the accepted window, newest last, dropping the oldest
    /// once it is full. A repeat of the newest entry is a no-op, so a mutation
    /// that leaves the root unchanged does not consume a slot.
    fn remember_root(&mut self, root: Field) {
        if self.recent_roots.back() == Some(&root) {
            return;
        }
        if self.recent_roots.len() == ROOT_HISTORY {
            self.recent_roots.pop_front();
        }
        self.recent_roots.push_back(root);
    }

    /// Undo `leaves` speculative inserts after a failed pipeline stage, and
    /// return the error to propagate.
    ///
    /// Rollback is sound only when the transaction provably did not land. On
    /// [`AppError::SubmitUnknown`] the mirror is parked instead: truncating would
    /// diverge it permanently if the transaction mines later, and keeping the
    /// leaves would diverge it if it never does.
    ///
    /// A rollback that itself fails is also a desync. Either way the original
    /// error is returned, since it explains the failure.
    #[must_use = "the returned error must be propagated"]
    pub fn unwind(&mut self, leaves: usize, cause: AppError) -> AppError {
        if let AppError::SubmitUnknown(reason) = &cause {
            error!(chain_id = self.chain_id, error = %cause, "submit outcome unknown; parking mirror");
            self.park(reason.clone());
            return cause;
        }
        error!(chain_id = self.chain_id, error = %cause, "stage failed; rolling back mirror");
        if let Err(e) = self.rollback(leaves) {
            error!(chain_id = self.chain_id, error = %e, "rollback failed; parking mirror");
            self.park(e.to_string());
        }
        cause
    }

    /// Refuse further work on this chain until a restart re-bootstraps from
    /// the indexer. Only [`Self::unwind`] should need this.
    fn park(&mut self, reason: String) {
        self.desynced.get_or_insert(reason);
        self.publish();
    }

    pub fn is_desynced(&self) -> bool {
        self.desynced.is_some()
    }

    fn check_usable(&self) -> AppResult<()> {
        match &self.desynced {
            None => Ok(()),
            Some(reason) => Err(AppError::MirrorDesynced(format!(
                "chain {}: {} (restart to re-bootstrap once the indexer has caught up)",
                self.chain_id, reason
            ))),
        }
    }

    /// Resume this chain's tree, and check the result against the latest
    /// `tree_advances.new_root`.
    ///
    /// The frontier fmd-indexer already maintains in `tree_state` is the whole of
    /// what this mirror holds, so the normal path is one row read. Replaying
    /// `notes` is the fallback for a chain the indexer has not written yet.
    pub async fn bootstrap(&mut self, pool: &DbPool) -> AppResult<()> {
        info!(chain_id = self.chain_id, "tree mirror bootstrap start");
        let (tree, source) = match tree_state::load(pool, self.chain_id).await? {
            Some(row) => (self.resume_from(row)?, BootSource::TreeState),
            None => (self.replay_notes(pool).await?, BootSource::Notes),
        };
        // Assigned together: the checkpoint is where a rollback lands, so it must
        // never name a state older than the tree it is paired with.
        self.checkpoint = tree.clone();
        self.tree = tree;
        self.publish();

        // Seeds the accepted-root window from the chain's own advance history.
        // Without it a restart narrows the window to the current root, and a
        // wallet holding a proof against the previous one receives a 400 for a
        // payload the pool would have accepted. Newest first, so the head is also
        // the root the mirror must currently agree with.
        let history = tree_advances::recent_roots(pool, self.chain_id, ROOT_HISTORY as i64).await?;

        if let [latest, ..] = history.as_slice()
            && self.tree.root().as_slice() != latest
        {
            return Err(AppError::Internal(format!(
                "tree mirror diverges from chain on chain_id {}: {} holds {}, \
                 but the chain last published {}",
                self.chain_id,
                source.as_str(),
                field_to_hex(&self.tree.root()),
                hex::encode(latest),
            )));
        }
        for root in history.iter().rev() {
            if let Ok(f) = vec_to_field(root) {
                self.remember_root(f);
            }
        }

        info!(
            chain_id = self.chain_id,
            leaves = self.tree.leaf_count(),
            roots = self.recent_roots.len(),
            source = source.as_str(),
            "tree mirror ready"
        );
        Ok(())
    }

    /// Adopt fmd-indexer's stored frontier. `Frontier::resume` folds the slots
    /// and refuses a `root` column that disagrees, so the cross-check the two
    /// columns need lives in the constructor rather than here: a disagreement
    /// means they were written from different states, and folding onto that
    /// would put a root on the wire the chain never held.
    fn resume_from(&self, row: TreeStateRow) -> AppResult<Frontier> {
        // `leaf_count` is a signed column, so the conversion is checked rather
        // than cast: a negative would otherwise wrap to a count past capacity and
        // surface as the wrong complaint.
        let leaves = u64::try_from(row.leaf_count)
            .map_err(|_| self.tree_state_err(format!("negative leaf_count {}", row.leaf_count)))?;
        let slots = decode_frontier(DEPTH, &row.frontier).map_err(|e| self.tree_state_err(e))?;
        let root = vec_to_field(&row.root).map_err(|e| self.tree_state_err(e))?;
        Frontier::resume(DEPTH, leaves, slots, root).map_err(|e| self.tree_state_err(e))
    }

    /// A complaint about this chain's `tree_state` row, tagged with the chain so
    /// the boot failure names which one to look at.
    fn tree_state_err(&self, detail: impl Display) -> AppError {
        AppError::Internal(format!("tree_state chain {}: {detail}", self.chain_id))
    }

    /// Fold `notes` into a frontier, for a chain fmd-indexer has not written a
    /// `tree_state` row for yet.
    ///
    /// Folds each page with [`Frontier::extend`] rather than a `push` loop: the
    /// batch carries only completed groups up and folds the root once per page,
    /// which is O(N) over the chain's whole history against `DEPTH` hashes per
    /// leaf, and it holds a page rather than the 1.33 nodes per leaf a
    /// [`MerkleTree`] would materialise for the same answer.
    async fn replay_notes(&self, pool: &DbPool) -> AppResult<Frontier> {
        info!(
            chain_id = self.chain_id,
            "no stored tree state; replaying notes"
        );
        let mut tree = Frontier::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
        // `appended` doubles as the page cursor and, once the loop ends, the leaf
        // count; the page query itself lives in `repositories::notes`.
        let mut appended: i64 = 0;
        loop {
            let rows = notes::leaf_page(pool, self.chain_id, appended, LEAF_PAGE).await?;
            if rows.is_empty() {
                break;
            }

            // Check row contiguity sequentially, which is cheap, then hash leaves
            // in parallel: `leaf_hash` is a pure Poseidon call, independent per
            // row. `appended` carries the running leaf index across pages, so a gap at
            // a page boundary is caught like any other.
            for (i, row) in rows.iter().enumerate() {
                let expected = appended + i as i64;
                if row.leaf_index != expected {
                    return Err(AppError::Internal(format!(
                        "tree desync chain {}: notes row {} has leaf_index {}",
                        self.chain_id, expected, row.leaf_index
                    )));
                }
            }
            let leaves: Vec<Field> = rows
                .par_iter()
                .map(|row| {
                    let cm_f = vec_to_field(&row.cm)?;
                    let cv_x = bigdecimal_to_u256(&row.cv_dep_x)?;
                    let cv_y = bigdecimal_to_u256(&row.cv_dep_y)?;
                    leaf_hash(&cm_f, &[cv_x, cv_y])
                })
                .collect::<AppResult<Vec<Field>>>()?;
            appended += rows.len() as i64;
            tree.extend(leaves)
                .map_err(|e| AppError::Internal(e.to_string()))?;
        }
        Ok(tree)
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
        self.publish();
        Ok((
            ReservedSlot {
                start_index,
                old_root,
                old_frontier,
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

    /// Cross-check the in-memory mirror against the on-chain `currentRoot()`,
    /// catching database and chain divergence, such as an anvil redeploy without a
    /// database reset, before the first submission reverts `StaleOldRoot()`.
    pub async fn verify_chain_root(
        &self,
        rpc: &RpcEndpoint,
        pool_address_hex: &str,
    ) -> AppResult<()> {
        let pool_address = Address::from_str(pool_address_hex)
            .map_err(|e| AppError::Internal(format!("pool addr: {}", e)))?;
        let provider = ProviderBuilder::new().on_client(rpc.client());
        let masp = IMasp::new(pool_address, provider);
        let chain_root = masp
            .currentRoot()
            .call()
            .await
            .map_err(|e| AppError::Rpc(format!("currentRoot: {}", e)))?
            ._0;
        let local_root = self.current_root();
        if chain_root.0 != local_root {
            return Err(AppError::Internal(format!(
                "tree mirror diverges from chain {}: local={} chain={} (DB likely stale; reset notes/tree_advances for this chain)",
                self.chain_id,
                field_to_hex(&local_root),
                hex::encode(chain_root.0),
            )));
        }
        info!(
            chain_id = self.chain_id,
            root = field_to_hex(&local_root),
            "tree mirror matches chain root"
        );
        Ok(())
    }

    /// Undo `n` speculative leaves after a submission that provably never landed,
    /// where the node rejected the broadcast or the transaction reverted on chain.
    /// An ambiguous failure goes to `mark_desynced` instead.
    ///
    /// Restores the checkpoint rather than dropping leaves one by one: a frontier
    /// keeps no record of what it folded, so the copy taken at reserve is the
    /// only state a rollback can land on. `n` is therefore a check on the
    /// caller's intent rather than an amount -- asking for any other number is
    /// asking for a state neither this mirror nor the chain was ever in, which
    /// subsumes the old "past the start" case, since nothing before the
    /// checkpoint is reachable either.
    pub fn rollback(&mut self, n: usize) -> AppResult<()> {
        let before = self.tree.leaf_count();
        let speculative = before - self.checkpoint.leaf_count();
        if n as u64 != speculative {
            return Err(AppError::Internal(format!(
                "chain {}: rollback of {} leaves, but {} are speculative ({} committed); \
                 the mirror can only return to its last reserve",
                self.chain_id,
                n,
                speculative,
                self.checkpoint.leaf_count()
            )));
        }
        // Captured before the restore and retracted after it: the advance being
        // undone published a root the chain never held. Left in the accepted
        // window, a wallet that read it from `/chains` would pass
        // `check_known_root` and then revert `StaleOldRoot` on chain.
        let root = self.tree.root();
        self.tree = self.checkpoint.clone();
        self.forget_newest_root(&root);
        self.publish();
        info!(
            chain_id = self.chain_id,
            n,
            before,
            after = self.tree.leaf_count(),
            "tree mirror rollback"
        );
        Ok(())
    }
}

pub fn vec_to_field(v: &[u8]) -> AppResult<Field> {
    common_crypto::tree::field_from_bytes(v).map_err(|e| AppError::Internal(e.to_string()))
}

pub fn field_to_hex(f: &Field) -> String {
    format!("0x{}", hex::encode(f))
}

#[cfg(test)]
mod tests;
