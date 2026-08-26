//! Assemble a batch of raw events into the rows one consume tick may commit.
//!
//! Pure and synchronous: everything the plan needs is passed in, so the decision
//! is testable without a database.

use crate::domain::convert::u256_to_bigdecimal;
use crate::domain::error::FmdIndexerError;
use crate::repositories::notes::NewNote;
use crate::repositories::raw_events::RawEventRow;
use crate::repositories::spent_nullifiers::NewSpentNullifier;
use alloy::primitives::U256;
use chain_types::decode::{self, DecodedEvent};
use shared::entities::EventKind;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use tracing::{error, warn};

/// Bytes of clueBits the FMD filter reads off the front of every ciphertext.
/// A leaf whose ciphertext is shorter cannot be scanned, so it is not stored.
const CLUE_BITS_PREFIX: usize = 2;

/// The FMD payload one tree leaf carries.
///
/// `NoteCreated` supplies it inline; a `DepositFlushed` sources it from the
/// `DepositEscrowed` event that opened the deposit. The columns are the same
/// either way, so both paths build the note through [`LeafPayload::into_note`].
#[derive(Clone)]
pub struct LeafPayload {
    pub cm: Vec<u8>,
    pub clue_rx: U256,
    pub clue_ry: U256,
    pub eph_pub_x: U256,
    pub eph_pub_y: U256,
    pub ciphertext: Vec<u8>,
    pub cv_dep_x: U256,
    pub cv_dep_y: U256,
}

impl LeafPayload {
    fn has_clue_bits(&self) -> bool {
        self.ciphertext.len() >= CLUE_BITS_PREFIX
    }

    /// Carries block coordinates only. A tx hash in the log would point from an
    /// operator's log stream straight into the note-to-deposit-to-payer join.
    fn into_note(self, chain_id: i64, row: &RawEventRow, leaf_index: i64) -> NewNote {
        NewNote {
            chain_id,
            block_number: row.block_number,
            tx_hash: row.tx_hash.clone(),
            log_index: row.log_index,
            cm: self.cm,
            clue_rx: u256_to_bigdecimal(self.clue_rx),
            clue_ry: u256_to_bigdecimal(self.clue_ry),
            eph_pub_x: u256_to_bigdecimal(self.eph_pub_x),
            eph_pub_y: u256_to_bigdecimal(self.eph_pub_y),
            ciphertext: self.ciphertext,
            leaf_index,
            cv_dep_x: u256_to_bigdecimal(self.cv_dep_x),
            cv_dep_y: u256_to_bigdecimal(self.cv_dep_y),
        }
    }
}

/// The two leaves one deposit mints, in the order `flushBatch` inserts them:
/// the depositor's note, then the note paying whoever flushed it.
///
/// The order is the leaf order the tree commits, so swapping them assigns both
/// notes the wrong `leaf_index` and every Merkle proof built against them fails.
#[derive(Clone)]
pub struct EscrowedLeaves {
    pub principal: LeafPayload,
    pub fee: LeafPayload,
}

/// Maps a deposit id, as a decimal string, to that deposit's two leaves.
pub type EscrowedMap = HashMap<String, EscrowedLeaves>;

/// Debug-printable: these are public chain values, and a plan is the first thing
/// to dump when a tick commits something unexpected.
#[derive(Debug)]
pub struct CommitPlan {
    pub notes: Vec<NewNote>,
    pub spent_nfs: Vec<NewSpentNullifier>,
    pub last_event_id: i64,
    pub last_block_number: i64,
}

/// The leaf range one `RootAdvanced` announced.
#[derive(Clone, Copy)]
struct Root {
    start_index: u64,
    inserted: u64,
}

/// Whether a tx may be committed yet.
#[derive(PartialEq, Eq)]
enum TxState {
    /// Fully observed. Everything up to and including it can be committed.
    Ready,
    /// Still waiting on data outside this batch. The commit walk stops here,
    /// since committing a later transaction would advance the cursor past this
    /// one.
    Pending,
}

/// Per-tx accumulator linking a single `RootAdvanced` to its trailing leaf
/// events.
///
/// The contract emits `RootAdvanced` first, at a lower log index, followed by one
/// `NoteCreated` per inserted leaf. `flushBatch` inverts that, emitting one
/// `DepositFlushed` per deposit before its root, so a leaf may arrive before its
/// base index is known. It is stored holding its ordinal and [`Self::set_root`]
/// rebases it once the root lands.
///
/// Completion counts leaf events observed rather than notes produced. A leaf that
/// cannot become a note, whether undecodable or carrying a ciphertext too short
/// for clueBits, still counts, so one bad leaf leaves a hole in `leaf_index`
/// rather than parking the cursor on a transaction that can never satisfy
/// `notes.len() == inserted`.
struct PendingTx {
    root: Option<Root>,
    leaf_seen: u64,
    skipped: u64,
    notes: Vec<NewNote>,
    spent_nfs: Vec<NewSpentNullifier>,
    /// Highest row of this transaction seen so far. The cursor commits through
    /// these, so they advance for every row, including ones this batch could not
    /// use, which decoding rejects identically on every replay.
    last_id: i64,
    last_block: i64,
    /// Set when a `DepositFlushed` references an escrow that is not yet ingested.
    /// Unlike an unusable leaf this may still resolve, so the transaction waits.
    deferred: bool,
}

impl PendingTx {
    fn new(row: &RawEventRow) -> Self {
        Self {
            root: None,
            leaf_seen: 0,
            skipped: 0,
            notes: Vec::new(),
            spent_nfs: Vec::new(),
            last_id: row.id,
            last_block: row.block_number,
            deferred: false,
        }
    }

    fn observe(&mut self, row: &RawEventRow) {
        self.last_id = row.id;
        self.last_block = row.block_number;
    }

    /// Record the transaction's leaf range and rebase leaves that arrived before
    /// it.
    ///
    /// Errors on a second root: overwriting the range would renumber the first
    /// root's leaves onto the second one's, writing indices belonging to another
    /// range and colliding on `notes_chain_leaf_idx`. Deferring instead would
    /// wedge the chain, so the tick fails.
    fn set_root(&mut self, root: Root) -> Result<(), FmdIndexerError> {
        if self.root.is_some() {
            return Err(FmdIndexerError::Decode(
                "two RootAdvanced events in a single tx".into(),
            ));
        }
        for note in &mut self.notes {
            note.leaf_index += root.start_index as i64;
        }
        self.notes.reserve(root.inserted as usize);
        self.root = Some(root);
        Ok(())
    }

    /// Reserve the next leaf ordinal, or `None` if the root already accounted for
    /// every leaf it announced. A surplus leaf must not be counted, or the
    /// transaction could never reach `inserted` again.
    fn claim_leaf(&mut self) -> Option<u64> {
        if self.root.is_some_and(|r| self.leaf_seen >= r.inserted) {
            return None;
        }
        let ordinal = self.leaf_seen;
        self.leaf_seen += 1;
        Some(ordinal)
    }

    /// Absolute leaf index for a claimed ordinal, or the bare ordinal while the
    /// root is unknown, for [`Self::set_root`] to rebase.
    fn leaf_index(&self, ordinal: u64) -> i64 {
        self.root.map_or(ordinal, |r| r.start_index + ordinal) as i64
    }

    fn state(&self) -> TxState {
        if self.deferred {
            return TxState::Pending;
        }
        match self.root {
            // Every leaf the root announced is accounted for, usable or not.
            Some(root) if self.leaf_seen == root.inserted => TxState::Ready,
            // No root: committable only if the transaction produced no leaf
            // either, otherwise its root is beyond the batch boundary.
            None if self.leaf_seen == 0 => TxState::Ready,
            _ => TxState::Pending,
        }
    }
}

/// Whether a leaf source may arrive before its `RootAdvanced`.
///
/// `flushBatch` emits one `DepositFlushed` per deposit and then its root, so a
/// deposit legitimately precedes the base index it will be numbered from. A
/// `NoteCreated` always trails its root, so one that does not cannot be indexed.
#[derive(Clone, Copy)]
enum LeafOrder {
    /// Root first, leaves after. A leaf seen before the root is unusable.
    RootLeads,
    /// Leaves first, root after. The leaf holds its ordinal until rebased.
    LeafLeads,
}

/// Everything one event needs beyond the tx it belongs to.
struct RowCtx<'a> {
    chain_id: i64,
    row: &'a RawEventRow,
    escrowed: &'a EscrowedMap,
}

impl RowCtx<'_> {
    fn warn_leaf_dropped(&self, reason: &str) {
        warn!(
            chain_id = self.chain_id,
            block_number = self.row.block_number,
            log_index = self.row.log_index,
            reason,
            "leaf event dropped; leaf_index range will have a hole"
        );
    }
}

/// Group raw events by `tx_hash`, decode them, and produce a commit plan up to
/// the first transaction that is not fully observed. `None` when nothing is
/// ready.
///
/// `escrowed` holds pre-resolved `DepositEscrowed` payloads keyed by deposit id.
/// A `DepositFlushed` referencing a missing one defers its whole transaction
/// until the escrow event has been ingested.
pub fn plan_commit(
    rows: &[RawEventRow],
    chain_id: i64,
    after: i64,
    escrowed: &EscrowedMap,
) -> Result<Option<CommitPlan>, FmdIndexerError> {
    Batch::assemble(rows, chain_id, escrowed).map(|batch| batch.commit_through(chain_id, after))
}

/// Decoded rows grouped by transaction, in first-seen and therefore id order.
struct Batch {
    by_tx: HashMap<Vec<u8>, PendingTx>,
    order: Vec<Vec<u8>>,
}

impl Batch {
    fn assemble(
        rows: &[RawEventRow],
        chain_id: i64,
        escrowed: &EscrowedMap,
    ) -> Result<Self, FmdIndexerError> {
        let mut batch = Self {
            by_tx: HashMap::new(),
            order: Vec::new(),
        };

        for row in rows {
            let tx = batch.tx_for(row);
            tx.observe(row);

            let Some(kind) = EventKind::from_i16(row.event_kind) else {
                // Not a leaf kind by construction: every leaf kind is known, so
                // this row cannot be one the transaction is waiting on.
                warn!(
                    chain_id,
                    event_kind = row.event_kind,
                    block_number = row.block_number,
                    log_index = row.log_index,
                    "unknown event kind; skipping"
                );
                continue;
            };

            let cx = RowCtx {
                chain_id,
                row,
                escrowed,
            };
            let decoded = match decode::decode(kind, &row.topics, &row.data) {
                Ok(decoded) => decoded,
                Err(e) => {
                    // A leaf that will not decode never will, so count it and let
                    // the transaction complete one leaf short.
                    if matches!(kind, EventKind::NoteCreated | EventKind::DepositFlushed) {
                        tx.claim_leaf();
                        tx.skipped += 1;
                        cx.warn_leaf_dropped("decode failed");
                    } else {
                        warn!(chain_id, block_number = row.block_number, log_index = row.log_index, error = %e, "decode failed; skipping");
                    }
                    continue;
                }
            };

            for event in decoded {
                tx.apply(event, &cx)?;
            }
        }
        Ok(batch)
    }

    fn tx_for(&mut self, row: &RawEventRow) -> &mut PendingTx {
        match self.by_tx.entry(row.tx_hash.clone()) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => {
                self.order.push(v.key().clone());
                v.insert(PendingTx::new(row))
            }
        }
    }

    /// Drain transactions in order until one is not [`TxState::Ready`].
    fn commit_through(mut self, chain_id: i64, after: i64) -> Option<CommitPlan> {
        let mut plan = CommitPlan {
            notes: Vec::new(),
            spent_nfs: Vec::new(),
            last_event_id: after,
            last_block_number: 0,
        };

        for tx_hash in &self.order {
            let tx = self.by_tx.get_mut(tx_hash).expect("assembled from `order`");
            if tx.state() == TxState::Pending {
                break;
            }
            if tx.skipped > 0 {
                error!(
                    chain_id,
                    tx_hash = %hex::encode(tx_hash),
                    block_number = tx.last_block,
                    skipped = tx.skipped,
                    "committing tx with unusable leaves; leaf_index range has holes"
                );
            }
            plan.last_event_id = tx.last_id;
            plan.last_block_number = tx.last_block;
            plan.notes.append(&mut tx.notes);
            plan.spent_nfs.append(&mut tx.spent_nfs);
        }

        (plan.last_event_id != after).then_some(plan)
    }
}

impl PendingTx {
    fn apply(&mut self, event: DecodedEvent, cx: &RowCtx<'_>) -> Result<(), FmdIndexerError> {
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
            ),

            DecodedEvent::NullifierConsumed { nf } => self.spent_nfs.push(NewSpentNullifier {
                chain_id: cx.chain_id,
                block_number: cx.row.block_number,
                log_index: cx.row.log_index,
                nf: nf.0.to_vec(),
                tx_hash: cx.row.tx_hash.clone(),
                block_ts: cx.row.block_ts,
            }),

            DecodedEvent::DepositFlushed { id, .. } => {
                let deposit_id = id.to_string();
                let Some(payload) = cx.escrowed.get(&deposit_id) else {
                    // The escrow event may not be ingested yet, so this is a wait
                    // rather than a drop. Logged because the wait is unbounded: if
                    // the escrow log predates the ingester's start block it never
                    // arrives and the chain stops here.
                    warn!(
                        chain_id = cx.chain_id,
                        deposit_id,
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
                self.push_leaf(cx, leaves.principal, LeafOrder::LeafLeads);
                self.push_leaf(cx, leaves.fee, LeafOrder::LeafLeads);
            }

            _ => {}
        }
        Ok(())
    }

    /// Claim a leaf ordinal and store the note, or account for the leaf as a
    /// hole. Either way the ordinal is consumed, so the transaction's leaf count
    /// stays reconcilable against `inserted`.
    fn push_leaf(&mut self, cx: &RowCtx<'_>, payload: LeafPayload, order: LeafOrder) {
        let Some(ordinal) = self.claim_leaf() else {
            warn!(
                chain_id = cx.chain_id,
                block_number = cx.row.block_number,
                log_index = cx.row.log_index,
                "leaf event beyond the root's inserted count; ignoring"
            );
            return;
        };

        let unusable = match order {
            LeafOrder::RootLeads if self.root.is_none() => Some("no RootAdvanced yet"),
            _ if !payload.has_clue_bits() => Some("ciphertext too short for clueBits prefix"),
            _ => None,
        };
        if let Some(reason) = unusable {
            self.skipped += 1;
            cx.warn_leaf_dropped(reason);
            return;
        }

        let leaf_index = self.leaf_index(ordinal);
        self.notes
            .push(payload.into_note(cx.chain_id, cx.row, leaf_index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{B256, Bytes, LogData};
    use alloy::sol_types::SolEvent;
    use chain_types::abi::{DepositFlushed, NotePayload, NullifierConsumed, RootAdvanced};

    const CHAIN: i64 = 1;
    /// Cursor position the batch starts from. `plan_commit` reports `None` while
    /// nothing has moved past it.
    const AFTER: i64 = 100;

    /// Builds the row stream `plan_commit` reads, with ids and log indices
    /// assigned in emission order as the ingester writes them.
    #[derive(Default)]
    struct Rows(Vec<RawEventRow>);

    impl Rows {
        fn push(&mut self, tx: u8, block: i64, kind: EventKind, log: LogData) -> &mut Self {
            let seq = self.0.len() as i64;
            self.0.push(RawEventRow {
                id: AFTER + 1 + seq,
                chain_id: CHAIN,
                block_number: block,
                block_hash: vec![0xaa; 32],
                block_ts: 1_700_000_000 + block,
                tx_hash: vec![tx; 32],
                log_index: seq as i32,
                event_kind: kind.as_i16(),
                topics: log.topics().iter().map(|t| t.0.to_vec()).collect(),
                data: log.data.to_vec(),
            });
            self
        }

        fn root(&mut self, tx: u8, block: i64, start_index: u64, inserted: u64) -> &mut Self {
            let ev = RootAdvanced {
                startIndex: start_index,
                inserted,
                oldRoot: B256::repeat_byte(0xee),
                newRoot: B256::repeat_byte(0xff),
            };
            self.push(tx, block, EventKind::RootAdvanced, ev.encode_log_data())
        }

        fn note(&mut self, tx: u8, block: i64, cm: u8, ciphertext: Vec<u8>) -> &mut Self {
            let ev = NotePayload {
                cm: B256::repeat_byte(cm),
                clueRx: U256::from(1u64),
                clueRy: U256::from(2u64),
                ephPubX: U256::ZERO,
                ephPubY: U256::ZERO,
                ciphertext: Bytes::from(ciphertext),
                cvDepX: U256::ZERO,
                cvDepY: U256::ZERO,
            };
            self.push(tx, block, EventKind::NoteCreated, ev.encode_log_data())
        }

        fn nullifier(&mut self, tx: u8, block: i64, nf: u8) -> &mut Self {
            let ev = NullifierConsumed {
                nf: B256::repeat_byte(nf),
            };
            self.push(
                tx,
                block,
                EventKind::NullifierConsumed,
                ev.encode_log_data(),
            )
        }

        fn flushed(&mut self, tx: u8, block: i64, deposit_id: u64) -> &mut Self {
            let ev = DepositFlushed {
                id: U256::from(deposit_id),
                cm: B256::repeat_byte(0x11),
            };
            self.push(tx, block, EventKind::DepositFlushed, ev.encode_log_data())
        }

        fn plan(&self, escrowed: &EscrowedMap) -> Option<CommitPlan> {
            plan_commit(&self.0, CHAIN, AFTER, escrowed).expect("no invariant violation")
        }

        fn plan_bare(&self) -> Option<CommitPlan> {
            self.plan(&EscrowedMap::new())
        }
    }

    /// A ciphertext long enough to carry the 2-byte clueBits prefix.
    fn usable_ciphertext() -> Vec<u8> {
        vec![0x00, 0x07, 0xde, 0xad]
    }

    fn escrow(deposit_id: u64, ciphertext: Vec<u8>) -> EscrowedMap {
        let leaf = |tag: u8, ciphertext: Vec<u8>| LeafPayload {
            cm: vec![tag; 32],
            clue_rx: U256::from(1u64),
            clue_ry: U256::from(2u64),
            eph_pub_x: U256::ZERO,
            eph_pub_y: U256::ZERO,
            ciphertext,
            cv_dep_x: U256::ZERO,
            cv_dep_y: U256::ZERO,
        };
        let leaves = EscrowedLeaves {
            principal: leaf(deposit_id as u8, ciphertext.clone()),
            // Distinct `cm` so a test cannot pass by committing the same leaf
            // twice.
            fee: leaf(deposit_id as u8 ^ 0xff, ciphertext),
        };
        EscrowedMap::from([(deposit_id.to_string(), leaves)])
    }

    fn leaf_indices(plan: &CommitPlan) -> Vec<i64> {
        plan.notes.iter().map(|n| n.leaf_index).collect()
    }

    #[test]
    fn a_complete_tx_commits_with_contract_assigned_leaf_indices() {
        let mut rows = Rows::default();
        rows.root(0x01, 10, 64, 2)
            .note(0x01, 10, 0xa0, usable_ciphertext())
            .note(0x01, 10, 0xa1, usable_ciphertext());

        let plan = rows.plan_bare().expect("tx is complete");

        assert_eq!(leaf_indices(&plan), [64, 65]);
        assert_eq!(plan.last_event_id, AFTER + 3, "cursor clears the whole tx");
        assert_eq!(plan.last_block_number, 10);
    }

    #[test]
    fn a_tx_straddling_the_batch_boundary_is_deferred() {
        // The root announces two leaves but only one is in this window; the
        // second is in the next batch, so nothing here may commit.
        let mut rows = Rows::default();
        rows.root(0x01, 10, 0, 2)
            .note(0x01, 10, 0xa0, usable_ciphertext());

        assert!(rows.plan_bare().is_none());
    }

    #[test]
    fn an_unusable_leaf_leaves_a_hole_and_still_commits() {
        // A ciphertext too short to carry clueBits can never become a note.
        // Completion counts leaf events, so the transaction still clears: the
        // hole is at index 1 and leaf 2 keeps the index the contract gave it.
        let mut rows = Rows::default();
        rows.root(0x01, 10, 0, 3)
            .note(0x01, 10, 0xa0, usable_ciphertext())
            .note(0x01, 10, 0xa1, vec![0x00])
            .note(0x01, 10, 0xa2, usable_ciphertext());

        let plan = rows.plan_bare().expect("commits despite the hole");

        assert_eq!(leaf_indices(&plan), [0, 2]);
    }

    #[test]
    fn a_leaf_beyond_the_root_count_is_ignored_without_blocking_the_tx() {
        // A surplus leaf must not be counted, or the transaction could never match
        // `inserted` again and the chain would park on it.
        let mut rows = Rows::default();
        rows.root(0x01, 10, 0, 1)
            .note(0x01, 10, 0xa0, usable_ciphertext())
            .note(0x01, 10, 0xa1, usable_ciphertext());

        let plan = rows.plan_bare().expect("tx is complete at one leaf");

        assert_eq!(leaf_indices(&plan), [0]);
    }

    #[test]
    fn a_spend_only_tx_commits_and_reports_its_own_block() {
        // No leaves, so no RootAdvanced. Requiring one would wedge the chain, and
        // sourcing the block from the last note would report 0.
        let mut rows = Rows::default();
        rows.nullifier(0x01, 500, 0xb0).nullifier(0x01, 500, 0xb1);

        let plan = rows.plan_bare().expect("nothing left to wait for");

        assert!(plan.notes.is_empty());
        assert_eq!(plan.spent_nfs.len(), 2);
        assert_eq!(plan.last_block_number, 500);
    }

    #[test]
    fn deposits_emitted_before_their_root_are_rebased_onto_it() {
        // `flushBatch` inverts the usual order: leaves first, root after, so each
        // deposit holds its ordinal until the root supplies the base.
        //
        // Two deposits, four leaves: each mints its own note plus the note paying
        // whoever flushed it, so `inserted` is twice the deposit count.
        let mut rows = Rows::default();
        rows.flushed(0x01, 10, 7)
            .flushed(0x01, 10, 8)
            .root(0x01, 10, 32, 4);

        let mut escrowed = escrow(7, usable_ciphertext());
        escrowed.extend(escrow(8, usable_ciphertext()));

        let plan = rows.plan(&escrowed).expect("tx is complete");

        assert_eq!(leaf_indices(&plan), [32, 33, 34, 35]);
    }

    #[test]
    fn a_deposit_whose_escrow_is_not_ingested_defers_its_tx() {
        let mut rows = Rows::default();
        rows.flushed(0x01, 10, 7).root(0x01, 10, 0, 2);

        assert!(rows.plan_bare().is_none(), "waits for the escrow event");

        // Resolves once the escrow lands, with no other change. One
        // `DepositFlushed`, two leaves: the escrow event carries both.
        let plan = rows
            .plan(&escrow(7, usable_ciphertext()))
            .expect("resolved");
        assert_eq!(leaf_indices(&plan), [0, 1]);
    }

    #[test]
    fn a_deferred_tx_holds_back_the_complete_ones_behind_it() {
        // Committing tx 2 would advance the cursor past tx 1, which is still
        // waiting, so tx 1's events would never be read again.
        let mut rows = Rows::default();
        rows.flushed(0x01, 10, 7)
            .root(0x01, 10, 0, 2)
            .root(0x02, 11, 2, 1)
            .note(0x02, 11, 0xa0, usable_ciphertext());

        assert!(rows.plan_bare().is_none());
    }

    #[test]
    fn a_second_root_in_one_tx_is_an_error_rather_than_a_wrong_index() {
        // Overwriting the range would renumber the first root's leaf onto the
        // second one's, writing an index that belongs elsewhere and colliding on
        // `notes_chain_leaf_idx`.
        let mut rows = Rows::default();
        rows.root(0x01, 10, 0, 1)
            .note(0x01, 10, 0xa0, usable_ciphertext())
            .root(0x01, 10, 64, 1);

        let err = plan_commit(&rows.0, CHAIN, AFTER, &EscrowedMap::new())
            .expect_err("invariant violation must surface");
        assert!(err.to_string().contains("RootAdvanced"), "got: {err}");
    }

    #[test]
    fn an_empty_batch_commits_nothing() {
        assert!(Rows::default().plan_bare().is_none());
    }
}
