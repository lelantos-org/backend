//! The per-transaction accumulator: roots, the leaves they number, and whether
//! the transaction may be committed yet.

use super::NewSpentNullifier;
use super::leaf::{LeafPayload, TreeLeaf};
use crate::domain::error::FmdIndexerError;
use crate::domain::escrow::EscrowedMap;
use chain_types::decode::DecodedEvent;
use database::models::{NewNote, RawEventRow};
use tracing::warn;

/// The leaf range one `RootAdvanced` announced.
#[derive(Clone, Copy)]
pub(super) struct Root {
    pub(super) start_index: u64,
    pub(super) inserted: u64,
}

/// Whether a tx may be committed yet.
#[derive(PartialEq, Eq)]
pub(super) enum TxState {
    /// Fully observed. Everything up to and including it can be committed.
    Ready,
    /// Still waiting on data outside this batch. The commit walk stops here,
    /// since committing a later transaction would advance the cursor past this
    /// one.
    Pending,
}

/// Per-tx accumulator linking the transaction's `RootAdvanced` events to their
/// leaf events.
///
/// A `Bundler` transaction carries one `RootAdvanced` per tree-advancing item,
/// each starting where the previous one ended, so the leaves of the whole
/// transaction form one contiguous range from the first root's `start_index`.
/// A leaf is numbered by its position in the transaction, not by which root it
/// belongs to.
///
/// A spend emits `RootAdvanced` first, at a lower log index, followed by one
/// `NoteCreated` per inserted leaf. `flushBatch` inverts that, emitting one
/// `DepositFlushed` per deposit before its root, so a leaf may arrive before its
/// base index is known. It is stored holding its ordinal and [`Self::set_root`]
/// rebases it once the first root lands.
///
/// Completion counts leaf events observed rather than notes produced. A leaf that
/// cannot become a note, whether undecodable or carrying a ciphertext too short
/// for clueBits, still counts, so one bad leaf leaves a hole in `leaf_index`
/// rather than parking the cursor on a transaction that can never satisfy
/// `notes.len() == inserted`.
pub(super) struct PendingTx {
    /// Every root seen so far, contiguous by construction.
    pub(super) roots: Vec<Root>,
    pub(super) leaf_seen: u64,
    pub(super) skipped: u64,
    pub(super) notes: Vec<NewNote>,
    /// Every leaf the roots announced, including those `notes` had to drop. This
    /// is what advances the stored tree frontier.
    pub(super) leaves: Vec<TreeLeaf>,
    pub(super) spent_nfs: Vec<NewSpentNullifier>,
    /// Highest row of this transaction seen so far. The cursor commits through
    /// these, so they advance for every row, including ones this batch could not
    /// use, which decoding rejects identically on every replay.
    pub(super) last_id: i64,
    pub(super) last_block: i64,
    /// Set when a `DepositFlushed` references an escrow that is not yet ingested.
    /// Unlike an unusable leaf this may still resolve, so the transaction waits.
    pub(super) deferred: bool,
}

impl PendingTx {
    pub(super) fn new(row: &RawEventRow) -> Self {
        Self {
            roots: Vec::new(),
            leaf_seen: 0,
            skipped: 0,
            notes: Vec::new(),
            leaves: Vec::new(),
            spent_nfs: Vec::new(),
            last_id: row.id,
            last_block: row.block_number,
            deferred: false,
        }
    }

    pub(super) fn observe(&mut self, row: &RawEventRow) {
        self.last_id = row.id;
        self.last_block = row.block_number;
    }

    /// Record a root's leaf range, rebasing leaves that arrived before the
    /// transaction's first one.
    ///
    /// Errors on a root that does not start where the previous one ended: the
    /// contract chains every item onto the last, so a gap or overlap means the
    /// tx-wide ordinals no longer map onto the tree, and numbering them anyway
    /// would write indices belonging to another range and collide on
    /// `notes_chain_leaf_idx`. Deferring instead would wedge the chain, so the
    /// tick fails.
    pub(super) fn set_root(&mut self, root: Root) -> Result<(), FmdIndexerError> {
        match self.roots.last() {
            None => {
                let base = root.start_index as i64;
                for note in &mut self.notes {
                    note.leaf_index += base;
                }
                for leaf in &mut self.leaves {
                    leaf.leaf_index += base;
                }
            }
            Some(prev) if root.start_index != prev.start_index + prev.inserted => {
                return Err(FmdIndexerError::Decode(format!(
                    "non-contiguous RootAdvanced events in a single tx: {} + {} then {}",
                    prev.start_index, prev.inserted, root.start_index
                )));
            }
            Some(_) => {}
        }
        self.notes.reserve(root.inserted as usize);
        self.leaves.reserve(root.inserted as usize);
        self.roots.push(root);
        Ok(())
    }

    /// Leaves announced by every root seen so far.
    pub(super) fn inserted(&self) -> u64 {
        self.roots.iter().map(|r| r.inserted).sum()
    }

    /// Reserve the next leaf ordinal, or `None` for a `NoteCreated` the roots
    /// seen so far have no room left for. Such a leaf is surplus, since its root
    /// always precedes it, and must not be counted, or the transaction could
    /// never reach `inserted` again. A `DepositFlushed` is never refused: its
    /// root is still to come.
    pub(super) fn claim_leaf(&mut self, order: LeafOrder) -> Option<u64> {
        if matches!(order, LeafOrder::RootLeads) && self.leaf_seen >= self.inserted() {
            return None;
        }
        let ordinal = self.leaf_seen;
        self.leaf_seen += 1;
        Some(ordinal)
    }

    /// Absolute leaf index for a claimed ordinal, or the bare ordinal while no
    /// root is known, for [`Self::set_root`] to rebase.
    pub(super) fn leaf_index(&self, ordinal: u64) -> i64 {
        self.roots
            .first()
            .map_or(ordinal, |r| r.start_index + ordinal) as i64
    }

    /// Ready when every leaf the roots announced is accounted for, usable or
    /// not. With no root that means no leaf either, otherwise its root is beyond
    /// the batch boundary.
    ///
    /// This is also what lets a batch window cut a `Bundler` transaction: the
    /// counts only meet between items, where every root seen has all its leaves
    /// and no flushed deposit is waiting on its root, and every intermediate root
    /// is one the chain registered. The rest of the transaction is read next tick
    /// as a transaction of its own, numbered from its own first root.
    pub(super) fn state(&self) -> TxState {
        if self.deferred || self.leaf_seen != self.inserted() {
            TxState::Pending
        } else {
            TxState::Ready
        }
    }
}

/// Whether a leaf source may arrive before its `RootAdvanced`.
///
/// `flushBatch` emits one `DepositFlushed` per deposit and then its root, so a
/// deposit legitimately precedes the base index it will be numbered from. A
/// `NoteCreated` always trails its root, so one that does not cannot be indexed.
#[derive(Clone, Copy)]
pub(super) enum LeafOrder {
    /// Root first, leaves after. A leaf no root seen so far has room for is
    /// ignored.
    RootLeads,
    /// Leaves first, root after. The leaf holds its ordinal until rebased.
    LeafLeads,
}

/// Everything one event needs beyond the tx it belongs to.
pub(super) struct RowCtx<'a> {
    pub(super) chain_id: i64,
    pub(super) row: &'a RawEventRow,
    pub(super) escrowed: &'a EscrowedMap,
}

impl RowCtx<'_> {
    pub(super) fn warn_leaf_surplus(&self) {
        warn!(
            chain_id = self.chain_id,
            block_number = self.row.block_number,
            log_index = self.row.log_index,
            "leaf event beyond the roots' inserted count; ignoring"
        );
    }

    pub(super) fn warn_leaf_dropped(&self, reason: &str) {
        warn!(
            chain_id = self.chain_id,
            block_number = self.row.block_number,
            log_index = self.row.log_index,
            reason,
            "leaf event dropped; leaf_index range will have a hole"
        );
    }
}

impl PendingTx {
    pub(super) fn apply(
        &mut self,
        event: DecodedEvent,
        cx: &RowCtx<'_>,
    ) -> Result<(), FmdIndexerError> {
        match event {
            DecodedEvent::RootAdvanced {
                start_index,
                inserted,
                ..
            } => self.set_root(Root {
                start_index,
                inserted,
            })?,

            DecodedEvent::NoteCreated {
                cm,
                clue_rx,
                clue_ry,
                eph_pub_x,
                eph_pub_y,
                ciphertext,
                cv_dep_x,
                cv_dep_y,
            } => self.push_leaf(
                cx,
                LeafPayload {
                    cm: cm.0.to_vec(),
                    clue_rx,
                    clue_ry,
                    eph_pub_x,
                    eph_pub_y,
                    ciphertext,
                    cv_dep_x,
                    cv_dep_y,
                },
                LeafOrder::RootLeads,
            )?,

            DecodedEvent::NullifierConsumed { nf } => self.spent_nfs.push(NewSpentNullifier {
                chain_id: cx.chain_id,
                block_number: cx.row.block_number,
                log_index: cx.row.log_index,
                nf: nf.0.to_vec(),
                tx_hash: cx.row.tx_hash.clone(),
                block_ts: cx.row.block_ts,
            }),

            DecodedEvent::DepositFlushed { id, .. } => {
                let Some(payload) = cx.escrowed.get(&id) else {
                    // The escrow event may not be ingested yet, so this is a wait
                    // rather than a drop. Logged because the wait is unbounded: if
                    // the escrow log predates the ingester's start block it never
                    // arrives and the chain stops here.
                    warn!(
                        chain_id = cx.chain_id,
                        deposit_id = %id,
                        block_number = cx.row.block_number,
                        log_index = cx.row.log_index,
                        "DepositEscrowed not ingested; deferring tx"
                    );
                    self.deferred = true;
                    return Ok(());
                };
                // Two leaves, of which `DepositFlushed` announces only the first:
                // the contract emits once per deposit while inserting both, so the
                // fee leaf has no event of its own and would otherwise leave the
                // transaction's leaf count short of `inserted`.
                let leaves = payload.clone();
                self.push_leaf(cx, leaves.principal, LeafOrder::LeafLeads)?;
                self.push_leaf(cx, leaves.fee, LeafOrder::LeafLeads)?;
            }

            _ => {}
        }
        Ok(())
    }

    /// Claim a leaf ordinal for a leaf the indexer cannot store. A surplus leaf is
    /// refused exactly as [`Self::push_leaf`] refuses one, rather than counted.
    pub(super) fn claim_hole(&mut self, cx: &RowCtx<'_>, order: LeafOrder, reason: &str) {
        if self.claim_leaf(order).is_none() {
            cx.warn_leaf_surplus();
            return;
        }
        self.skipped += 1;
        cx.warn_leaf_dropped(reason);
    }

    /// Claim a leaf ordinal and store the note, or account for the leaf as a
    /// hole. Either way the ordinal is consumed, so the transaction's leaf count
    /// stays reconcilable against `inserted`.
    pub(super) fn push_leaf(
        &mut self,
        cx: &RowCtx<'_>,
        payload: LeafPayload,
        order: LeafOrder,
    ) -> Result<(), FmdIndexerError> {
        let Some(ordinal) = self.claim_leaf(order) else {
            cx.warn_leaf_surplus();
            return Ok(());
        };

        let leaf_index = self.leaf_index(ordinal);
        // Recorded before the usability check below: the contract inserted this
        // leaf whatever the indexer can do with it, so the tree must advance past
        // it or every later leaf lands one position early.
        self.leaves.push(TreeLeaf {
            leaf_index,
            hash: payload.tree_hash()?,
        });

        // A `NoteCreated` with no root to number it was refused by `claim_leaf`
        // above, so only the payload itself can make a claimed leaf unusable.
        if !payload.has_clue_bits() {
            self.skipped += 1;
            cx.warn_leaf_dropped("ciphertext too short for clueBits prefix");
            return Ok(());
        }

        self.notes
            .push(payload.into_note(cx.chain_id, cx.row, leaf_index));
        Ok(())
    }
}
