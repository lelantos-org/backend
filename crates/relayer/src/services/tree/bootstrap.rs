//! Where a mirror's state comes from: fmd-indexer's tables at boot and on
//! resync, cross-checked against the pool itself.

use super::leaf::leaf_hash;
use super::{DEPTH, ROOT_HISTORY, TreeMirror, empty_root, field_to_hex, vec_to_field};
use crate::adapters::masp::MaspReader;
use crate::domain::error::{AppError, AppResult};
use crate::repositories::{notes, tree_advances, tree_state};
use ::asset_registry::bigdecimal_to_u256;
use crypto::tree::{Field, Frontier, decode_frontier};
use database::DbPool;
use database::models::TreeStateRow;
use rayon::prelude::*;
use std::collections::VecDeque;
use std::fmt::Display;
use tracing::info;

/// Leaves read per round trip during [`TreeMirror::bootstrap`].
const LEAF_PAGE: i64 = 100_000;

/// Where a mirror's starting state came from.
///
/// Worth naming in the boot log and in a divergence error: the two paths fail
/// for different reasons. A stale `tree_state` row means the indexer is behind,
/// which time fixes; a replay that diverges means `notes` disagrees with the
/// chain, which it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BootSource {
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

impl TreeMirror {
    /// Re-adopt the chain's state after it moved without this mirror: another
    /// relayer's bundle, or a third party's `flushBatch`.
    ///
    /// Parks first, so nothing reserves against the stale tree, then bootstraps
    /// from the indexer and checks the result against the pool. Stays parked if
    /// either disagrees, which is the normal state while the indexer is still
    /// behind the chain; the caller retries.
    pub async fn resync(
        &mut self,
        pool: &DbPool,
        masp: &MaspReader,
        reason: &str,
    ) -> AppResult<()> {
        self.bundle = None;
        self.park(reason.to_string());
        self.bootstrap(pool).await?;
        self.verify_chain_root(masp).await?;
        self.desynced = None;
        self.publish();
        info!(
            chain_id = self.chain_id,
            leaves = self.tree.leaf_count(),
            "tree mirror resynced with the chain"
        );
        Ok(())
    }

    /// Resume this chain's tree, and check the result against the latest
    /// `tree_advances.new_root`.
    ///
    /// The frontier fmd-indexer already maintains in `tree_state` is the whole of
    /// what this mirror holds, so the normal path is one row read. Replaying
    /// `notes` is the fallback for a chain the indexer has not written yet.
    pub async fn bootstrap(&mut self, pool: &DbPool) -> AppResult<()> {
        info!(chain_id = self.chain_id, "tree mirror bootstrap start");
        // Unknown until `verify_chain_root` reads it; a failed bootstrap must not
        // leave a position counted against a window it replaced.
        self.ring_index = None;
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
        self.adopt_history(&history, source)?;

        info!(
            chain_id = self.chain_id,
            leaves = self.tree.leaf_count(),
            roots = self.recent_roots.len(),
            source = source.as_str(),
            "tree mirror ready"
        );
        Ok(())
    }

    /// Replace the accepted-root window with the pool's ring as `history`, the
    /// newest `ROOT_HISTORY` advances' roots newest first, describes it.
    ///
    /// Rebuilt rather than appended to, so the window is the ring in order and an
    /// entry's age locates its slot. Fewer rows than the ring holds means every
    /// advance since genesis, so the ring still holds the empty root too, in slot
    /// 0. The ring position itself is left unknown until
    /// [`Self::verify_chain_root`] reads it from the pool.
    pub(super) fn adopt_history(
        &mut self,
        history: &[Vec<u8>],
        source: BootSource,
    ) -> AppResult<()> {
        let history = history
            .iter()
            .map(|r| vec_to_field(r))
            .collect::<AppResult<Vec<Field>>>()?;
        let mut window: VecDeque<Field> = VecDeque::with_capacity(ROOT_HISTORY);
        if history.len() < ROOT_HISTORY {
            window.push_back(empty_root()?);
        }
        window.extend(history.iter().rev());
        if window.back() != Some(&self.tree.root()) {
            return Err(AppError::Internal(format!(
                "tree mirror diverges from chain on chain_id {}: {} holds {}, \
                 but the chain last published {}",
                self.chain_id,
                source.as_str(),
                field_to_hex(&self.tree.root()),
                window.back().map_or_else(|| "nothing".into(), field_to_hex),
            )));
        }
        self.recent_roots = window;
        self.ring_index = None;
        Ok(())
    }

    /// Take the pool's `rootIndex`, the ring slot of the newest window entry.
    ///
    /// A window shorter than the ring spans every advance since genesis, so its
    /// newest entry must sit at slot `len - 1`; anything else means the indexer's
    /// history is missing advances, and every anchor computed from it would be
    /// wrong.
    pub(super) fn adopt_ring_index(&mut self, chain_index: u32) -> AppResult<()> {
        let chain_index = chain_index as usize;
        let len = self.recent_roots.len();
        if chain_index >= ROOT_HISTORY || (len < ROOT_HISTORY && chain_index != len - 1) {
            return Err(AppError::Internal(format!(
                "tree mirror diverges from chain {}: the pool's rootIndex is {chain_index}, \
                 but the indexer's history spans {len} roots",
                self.chain_id,
            )));
        }
        self.ring_index = Some(chain_index);
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
    /// [`MerkleTree`](crypto::tree::MerkleTree) would materialise for the same answer.
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

    /// Cross-check the in-memory mirror against the pool's current root, catching
    /// database and chain divergence, such as an anvil redeploy without a database
    /// reset, before the first submission reverts.
    ///
    /// Also adopts the pool's `rootIndex()`, which every spend's anchor slot is
    /// counted from; see [`MaspReader::newest_ring_slot`] for why the two are read
    /// together.
    pub async fn verify_chain_root(&mut self, masp: &MaspReader) -> AppResult<()> {
        let (ring_index, chain_root) = masp.newest_ring_slot().await?;
        let local_root = self.current_root();
        if chain_root != local_root {
            return Err(AppError::Internal(format!(
                "tree mirror diverges from chain {}: local={} chain={} (DB likely stale; reset notes/tree_advances for this chain)",
                self.chain_id,
                field_to_hex(&local_root),
                hex::encode(chain_root),
            )));
        }
        self.adopt_ring_index(ring_index)?;
        info!(
            chain_id = self.chain_id,
            root = field_to_hex(&local_root),
            ring_index,
            "tree mirror matches chain root"
        );
        Ok(())
    }
}
