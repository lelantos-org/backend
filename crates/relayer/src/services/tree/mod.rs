//! Per-chain in-memory tree mirror, bootstrapped from the existing `notes` and
//! `tree_advances` rows at startup. The relayer is otherwise stateless across
//! restarts and owns no tables.
//!
//! Each chain owns one `Arc<Mutex<TreeMirror>>`. The pipeline holds the mutex
//! through reserve, prove, submit and receipt, so the next bundle builds
//! optimistically against the post-state. A revert unwinds the speculative
//! inserts.

use crate::adapters::abi::IMasp;
use crate::adapters::numeric::bigdecimal_to_u256;
use crate::adapters::rpc::RpcEndpoint;
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use database::DbPool;
use database::models::LeafInputsRow;
use database::schema::{notes, tree_advances};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use fmd_crypto::poseidon as fmd_poseidon;
use fmd_crypto::tree::{Field, MerkleTree};
use rayon::prelude::*;
use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{error, info};

/// Merkle depth this mirror is built for.
///
/// Re-exported rather than declared: the depth is pinned by the circuits and
/// the verifier, so every service that mirrors the tree has to agree on one
/// value, and `fmd_crypto::tree` is the crate they all share.
pub use fmd_crypto::tree::DEPTH;
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
    fmd_poseidon::hash_bytes_be(&[&tag, cm, &cv_x, &cv_y])
        .map_err(|e| AppError::Internal(format!("leaf_hash: {}", e)))
}

pub struct TreeMirror {
    pub chain_id: i64,
    tree: MerkleTree,
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
        let tree = MerkleTree::new(DEPTH).map_err(|e| AppError::Internal(e.to_string()))?;
        let mut m = Self {
            chain_id,
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
        let root = self.tree.root().ok();
        self.snapshot
            .publish(self.tree.leaf_count() as u64, root, self.desynced.is_some());
        if let Some(root) = root {
            self.remember_root(root);
        }
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

    /// Replay every confirmed commitment from `notes` ordered by `leaf_index`, and
    /// check that the resulting root matches the latest `tree_advances.new_root`.
    pub async fn bootstrap(&mut self, pool: &DbPool) -> AppResult<()> {
        info!(chain_id = self.chain_id, "tree mirror bootstrap start");
        let mut conn = crate::repositories::conn(pool).await?;
        // Paged by `leaf_index` rather than loaded whole: a chain with millions of
        // notes would otherwise hold every leaf in one query result and one Vec
        // before the first hash runs.
        // Doubles as the page cursor and, once the loop ends, the leaf count.
        let mut appended: i64 = 0;
        loop {
            let rows: Vec<LeafInputsRow> = notes::table
                .filter(notes::chain_id.eq(self.chain_id))
                .filter(notes::leaf_index.ge(appended))
                .filter(notes::leaf_index.lt(appended + LEAF_PAGE))
                .order(notes::leaf_index.asc())
                .select(LeafInputsRow::as_select())
                .load(&mut conn)
                .await
                .map_err(|e| AppError::Db(e.to_string()))?;
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
            self.tree
                .extend(leaves)
                .map_err(|e| AppError::Internal(e.to_string()))?;
        }
        self.publish();

        // The latest `tree_advances.new_root` must match the local root.
        let latest: Option<(Vec<u8>, i64)> = tree_advances::table
            .filter(tree_advances::chain_id.eq(self.chain_id))
            .order(tree_advances::start_index.desc())
            .select((tree_advances::new_root, tree_advances::start_index))
            .first(&mut conn)
            .await
            .optional()
            .map_err(|e| AppError::Db(e.to_string()))?;
        // Seed the accepted-root window from the chain's own advance history.
        // Without it a restart narrows the window to the current root, and a
        // wallet holding a proof against the previous one receives a 400 for a
        // payload the pool would have accepted.
        let history: Vec<Vec<u8>> = tree_advances::table
            .filter(tree_advances::chain_id.eq(self.chain_id))
            .order(tree_advances::start_index.desc())
            .limit(ROOT_HISTORY as i64)
            .select(tree_advances::new_root)
            .load(&mut conn)
            .await
            .map_err(|e| AppError::Db(e.to_string()))?;
        for root in history.into_iter().rev() {
            if let Ok(f) = vec_to_field(&root) {
                self.remember_root(f);
            }
        }

        info!(
            chain_id = self.chain_id,
            leaves = appended,
            roots = self.recent_roots.len(),
            "tree mirror replayed notes"
        );
        if let Some((expected_root, _)) = latest {
            let local = self
                .tree
                .root()
                .map_err(|e| AppError::Internal(e.to_string()))?;
            if local.to_vec() != expected_root {
                return Err(AppError::Internal(format!(
                    "tree mirror diverges from chain on chain_id {}",
                    self.chain_id
                )));
            }
        }
        Ok(())
    }

    pub fn committed_count(&self) -> u64 {
        self.tree.leaf_count() as u64
    }

    pub fn current_root(&self) -> AppResult<Field> {
        self.tree
            .root()
            .map_err(|e| AppError::Internal(e.to_string()))
    }

    /// Insert `(cm, cv_dep)` pairs. The mirror hashes each pair into a leaf before
    /// insertion to stay in sync with the on-chain tree, which advances through
    /// SNARK-verified leaf roots.
    pub fn reserve_and_advance_batch(
        &mut self,
        cms: &[(Field, [U256; 2])],
    ) -> AppResult<(ReservedSlot, AdvancedState)> {
        self.check_usable()?;
        let start_index = self.tree.leaf_count() as u64;

        // Capacity first: a length check, so an oversized batch is refused without
        // computing a single Poseidon.
        if start_index as usize + cms.len() > MAX_LEAVES {
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

        let old_root = self
            .tree
            .root()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let old_frontier = self
            .tree
            .frontier()
            .map_err(|e| AppError::Internal(e.to_string()))?;

        // Past this point the tree is mutated, so any failure must be unwound
        // rather than propagated directly; see `insert_all`.
        let inserted = self.insert_all(leaves)?;
        debug_assert_eq!(inserted, cms.len());

        let new_root = self
            .tree
            .root()
            .map_err(|e| AppError::Internal(e.to_string()))?;
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
    /// `MerkleTree::insert` should not fail once capacity is checked, but a partial
    /// batch is the state that desyncs a mirror permanently, so it is undone here
    /// and the mirror parked if that also fails.
    fn insert_all(&mut self, leaves: Vec<Field>) -> AppResult<usize> {
        let n = leaves.len();
        for (i, leaf) in leaves.into_iter().enumerate() {
            if let Err(e) = self.tree.insert(leaf) {
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
        let local_root = self.current_root()?;
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
    pub fn rollback(&mut self, n: usize) -> AppResult<()> {
        let before = self.tree.leaf_count();
        if n > before {
            return Err(AppError::Internal(format!(
                "rollback {} > leaf_count {} on chain {}",
                n, before, self.chain_id
            )));
        }
        // Captured before the truncation and retracted after it: the advance being
        // undone published a root the chain never held. Left in the accepted
        // window, a wallet that read it from `/chains` would pass
        // `check_known_root` and then revert `StaleOldRoot` on chain.
        let speculative = self.tree.root().ok();
        self.tree
            .truncate_leaves(n)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        if let Some(root) = speculative {
            self.forget_newest_root(&root);
        }
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
    fmd_crypto::tree::field_from_bytes(v).map_err(|e| AppError::Internal(e.to_string()))
}

pub fn field_to_hex(f: &Field) -> String {
    format!("0x{}", hex::encode(f))
}

#[cfg(test)]
mod tests;
