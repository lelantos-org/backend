//! Undoing what the chain did not keep: a single batch's rollback, a bundle's
//! prefix commit, and parking a mirror whose state can no longer be trusted.

use super::TreeMirror;
use crate::domain::error::{AppError, AppResult};
use crypto::tree::{Field, Frontier};
use std::collections::VecDeque;
use tracing::{error, info};

/// Reserves made since [`TreeMirror::begin_bundle`], enough to keep any prefix of
/// them.
///
/// A frontier cannot drop leaves, so every item records the tree as it stood
/// before it. Keeping the first `j` items restores item `j`'s pre-state, and the
/// accepted-root window is rebuilt from where it stood when the bundle opened
/// plus the roots of the items kept.
pub(super) struct Bundle {
    pub(super) base_roots: VecDeque<Field>,
    pub(super) base_ring_index: Option<usize>,
    pub(super) items: Vec<BundleEntry>,
}

pub(super) struct BundleEntry {
    pub(super) before: Frontier,
    pub(super) new_root: Field,
}

impl TreeMirror {
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

    /// Refuse further work on this chain until [`Self::resync`] succeeds or a
    /// restart re-bootstraps from the indexer.
    pub(super) fn park(&mut self, reason: String) {
        self.desynced.get_or_insert(reason);
        self.publish();
    }

    /// Open a bundle: every reserve until [`Self::commit_prefix`] builds on the one
    /// before it and can be kept or discarded as a prefix.
    pub fn begin_bundle(&mut self) -> AppResult<()> {
        self.check_usable()?;
        if self.bundle.is_some() {
            return Err(AppError::Internal(format!(
                "chain {}: a bundle is already open",
                self.chain_id
            )));
        }
        self.bundle = Some(Bundle {
            base_roots: self.recent_roots.clone(),
            base_ring_index: self.ring_index,
            items: Vec::new(),
        });
        Ok(())
    }

    /// Items reserved in the open bundle, or 0 with none open.
    pub fn bundle_len(&self) -> usize {
        self.bundle.as_ref().map_or(0, |b| b.items.len())
    }

    /// How many of the open bundle's items it takes to reach `root`, or `None`
    /// when no non-empty prefix does or no bundle is open.
    pub fn bundle_prefix_reaching(&self, root: &Field) -> Option<usize> {
        let items = &self.bundle.as_ref()?.items;
        items
            .iter()
            .position(|e| e.new_root == *root)
            .map(|i| i + 1)
    }

    /// Close the open bundle, keeping its first `kept` items and discarding the
    /// rest.
    ///
    /// `kept == bundle_len()` keeps everything, which is a landed bundle;
    /// `kept == 0` is [`Self::rollback_bundle`]. Anything between is a bundle the
    /// chain stopped part-way through. The roots of discarded items leave the
    /// accepted window, since the chain never held them.
    pub fn commit_prefix(&mut self, kept: usize) -> AppResult<()> {
        let bundle = self.bundle.take().ok_or_else(|| {
            AppError::Internal(format!("chain {}: no bundle is open", self.chain_id))
        })?;
        let reserved = bundle.items.len();
        if kept > reserved {
            return Err(AppError::Internal(format!(
                "chain {}: cannot keep {kept} of {reserved} bundled items",
                self.chain_id
            )));
        }
        if kept == reserved {
            return Ok(());
        }
        let before = self.tree.leaf_count();
        self.tree = bundle.items[kept].before.clone();
        self.checkpoint = self.tree.clone();
        self.recent_roots = bundle.base_roots;
        self.ring_index = bundle.base_ring_index;
        for entry in &bundle.items[..kept] {
            self.remember_root(entry.new_root);
        }
        self.publish();
        info!(
            chain_id = self.chain_id,
            kept,
            reserved,
            before,
            after = self.tree.leaf_count(),
            "bundle prefix committed"
        );
        Ok(())
    }

    /// Discard every item of the open bundle.
    pub fn rollback_bundle(&mut self) -> AppResult<()> {
        self.commit_prefix(0)
    }

    /// Close the open bundle after a stage failed, as [`Self::unwind`] does for a
    /// single batch, and return the error to propagate.
    ///
    /// On [`AppError::SubmitUnknown`] the bundle may yet land, so its leaves stay
    /// and the mirror parks. Otherwise it provably did not, and is rolled back; a
    /// rollback that fails parks too.
    #[must_use = "the returned error must be propagated"]
    pub fn abandon_bundle(&mut self, cause: AppError) -> AppError {
        if let AppError::SubmitUnknown(reason) = &cause {
            error!(chain_id = self.chain_id, error = %cause, "bundle outcome unknown; parking mirror");
            self.bundle = None;
            self.park(reason.clone());
            return cause;
        }
        if self.bundle.is_some()
            && let Err(e) = self.rollback_bundle()
        {
            error!(chain_id = self.chain_id, error = %e, "bundle rollback failed; parking mirror");
            self.park(e.to_string());
        }
        cause
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
        // window, a wallet that read it from `/chains` would pass the batcher's
        // root check and then revert `UnknownRoot` on chain.
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
