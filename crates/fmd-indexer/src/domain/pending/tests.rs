//! Commit planning against hand-built row streams.

use super::batch::Batch;
use super::tx::TxState;
use super::*;
use crate::domain::escrow::EscrowedLeaves;
use alloy::primitives::U256;
use alloy::primitives::{Address, B256, Bytes, LogData};
use alloy::sol_types::SolEvent;
use chain_types::abi::{
    AssetMoved, DepositEscrowed, DepositFlushed, NotePayload, NullifierConsumed, RootAdvanced,
};
use shared::entities::EventKind;

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
        self.push_raw(tx, block, kind.as_i16(), log)
    }

    fn push_raw(&mut self, tx: u8, block: i64, event_kind: i16, log: LogData) -> &mut Self {
        let seq = self.0.len() as i64;
        self.0.push(RawEventRow {
            id: AFTER + 1 + seq,
            chain_id: CHAIN,
            block_number: block,
            block_hash: vec![0xaa; 32],
            block_ts: 1_700_000_000 + block,
            tx_hash: vec![tx; 32],
            log_index: seq as i32,
            event_kind,
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

    /// The four nullifiers a spend burns ahead of its root.
    fn nullifiers(&mut self, tx: u8, block: i64, tag: u8) -> &mut Self {
        for i in 0..4 {
            self.nullifier(tx, block, tag + i);
        }
        self
    }

    /// The six notes a spend emits after its root.
    fn notes(&mut self, tx: u8, block: i64, tag: u8) -> &mut Self {
        for i in 0..6 {
            self.note(tx, block, tag + i, usable_ciphertext());
        }
        self
    }

    fn asset_moved(&mut self, tx: u8, block: i64) -> &mut Self {
        let ev = AssetMoved {
            assetId: 1,
            token: Address::ZERO,
            inAmount: U256::ZERO,
            outAmount: U256::from(5u64),
            publicIn: 0,
            publicOut: 5,
        };
        self.push(tx, block, EventKind::AssetMoved, ev.encode_log_data())
    }

    /// The escrow a swap opens for its output. It inserts no leaf until a
    /// later flush.
    fn escrowed(&mut self, tx: u8, block: i64, deposit_id: u64) -> &mut Self {
        let ev = DepositEscrowed {
            id: U256::from(deposit_id),
            payer: Address::ZERO,
            recipient: Address::ZERO,
            publicAssetId: 1,
            publicIn: 5,
            feeBpsAtSubmit: 0,
            cm: B256::repeat_byte(0x33),
            cvDepX: U256::ZERO,
            cvDepY: U256::ZERO,
            rcv: U256::ZERO,
            clueRx: U256::from(1u64),
            clueRy: U256::from(2u64),
            ephPubX: U256::ZERO,
            ephPubY: U256::ZERO,
            ciphertext: Bytes::from(usable_ciphertext()),
            feeAssetId: 0,
            feeIn: 0,
            feeCm: B256::repeat_byte(0x34),
            feeCvDepX: U256::ZERO,
            feeCvDepY: U256::ZERO,
            feeRcv: U256::ZERO,
            feeClueRx: U256::from(1u64),
            feeClueRy: U256::from(2u64),
            feeEphPubX: U256::ZERO,
            feeEphPubY: U256::ZERO,
            feeCiphertext: Bytes::from(usable_ciphertext()),
        };
        self.push(tx, block, EventKind::DepositEscrowed, ev.encode_log_data())
    }

    /// A log of no kind this indexer knows, standing for another contract's
    /// log in the same transaction: an ERC-20 transfer, the native adapter's
    /// payout, the swap wrapper's or the bundler's own events.
    fn foreign(&mut self, tx: u8, block: i64) -> &mut Self {
        let log = LogData::new_unchecked(vec![B256::repeat_byte(0xdd)], Bytes::new());
        self.push_raw(tx, block, 0, log)
    }

    fn plan(&self, escrowed: &EscrowedMap) -> Option<CommitPlan> {
        plan_window(&self.0, AFTER, escrowed)
    }

    fn plan_bare(&self) -> Option<CommitPlan> {
        self.plan(&EscrowedMap::new())
    }
}

/// One tick's plan over `window`, with the cursor at `after`.
fn plan_window(window: &[RawEventRow], after: i64, escrowed: &EscrowedMap) -> Option<CommitPlan> {
    plan_commit(window, CHAIN, after, escrowed).expect("no invariant violation")
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
    EscrowedMap::from([(U256::from(deposit_id), leaves)])
}

fn leaf_indices(plan: &CommitPlan) -> Vec<i64> {
    plan.notes.iter().map(|n| n.leaf_index).collect()
}

fn tree_indices(plan: &CommitPlan) -> Vec<i64> {
    plan.leaves.iter().map(|l| l.leaf_index).collect()
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

/// A log of `kind` whose payload does not decode.
fn undecodable(rows: &mut Rows, tx: u8, block: i64, kind: EventKind) {
    let log = LogData::new_unchecked(vec![B256::repeat_byte(0xdd)], Bytes::new());
    rows.push(tx, block, kind, log);
}

#[test]
fn an_undecodable_flush_accounts_for_both_of_its_leaves() {
    // A flushed deposit inserts two leaves. Counting one would leave the
    // transaction a leaf short of `inserted` and park the chain on it forever.
    let mut rows = Rows::default();
    undecodable(&mut rows, 0x01, 10, EventKind::DepositFlushed);
    rows.flushed(0x01, 10, 7).root(0x01, 10, 8, 4);

    let plan = rows
        .plan(&escrow(7, usable_ciphertext()))
        .expect("the undecodable deposit's two leaves are holes, not a stall");

    assert_eq!(
        leaf_indices(&plan),
        [10, 11],
        "the decodable deposit follows both holes"
    );
}

#[test]
fn an_undecodable_surplus_note_is_ignored_not_counted_as_a_hole() {
    let mut rows = Rows::default();
    rows.root(0x01, 10, 0, 1)
        .note(0x01, 10, 0xa0, usable_ciphertext());
    undecodable(&mut rows, 0x01, 10, EventKind::NoteCreated);

    let batch = Batch::assemble(&rows.0, CHAIN, &EscrowedMap::new()).expect("assembles");
    let tx = batch.by_tx.values().next().expect("one tx");
    assert_eq!(tx.skipped, 0, "a leaf no root has room for is not a hole");
    assert!(tx.state() == TxState::Ready);
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
fn a_non_contiguous_root_in_one_tx_is_an_error_rather_than_a_wrong_index() {
    // The second root does not start where the first ended, so the tx-wide
    // ordinals no longer map onto the tree. Numbering them anyway would write
    // an index that belongs elsewhere and collide on `notes_chain_leaf_idx`.
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

#[test]
fn a_note_no_root_has_room_for_is_ignored_even_before_any_root() {
    // A `NoteCreated` always trails its root, so one with no root to number
    // it is surplus. Counting it would leave the tx a leaf over `inserted`
    // for good and park the chain on it.
    let mut rows = Rows::default();
    rows.note(0x01, 10, 0xa0, usable_ciphertext())
        .nullifier(0x01, 10, 0xb0);

    let plan = rows.plan_bare().expect("nothing left to wait for");

    assert!(plan.notes.is_empty() && plan.leaves.is_empty());
    assert_eq!(plan.spent_nfs.len(), 1);
}

#[test]
fn a_bundle_numbers_leaves_across_its_roots_whichever_side_they_lead() {
    // Spend, flush, spend in one `Bundler` tx: three roots, each starting
    // where the last ended. The spends' notes trail their roots, the flush's
    // deposit precedes its own, and together they are one contiguous range.
    let mut rows = Rows::default();
    rows.nullifiers(0x01, 10, 0xa0)
        .root(0x01, 10, 10, 6)
        .notes(0x01, 10, 0xa0)
        .flushed(0x01, 10, 7)
        .root(0x01, 10, 16, 2)
        .nullifiers(0x01, 10, 0xb0)
        .root(0x01, 10, 18, 6)
        .notes(0x01, 10, 0xb0);

    let plan = rows
        .plan(&escrow(7, usable_ciphertext()))
        .expect("tx is complete");

    assert_eq!(leaf_indices(&plan), (10..24).collect::<Vec<_>>());
    assert_eq!(tree_indices(&plan), leaf_indices(&plan));
    assert_eq!(plan.spent_nfs.len(), 8);
    assert_eq!(plan.last_event_id, AFTER + rows.0.len() as i64);
}

#[test]
fn consecutive_flushes_in_one_tx_each_rebase_onto_their_own_root() {
    // Only leaves ahead of the first root hold a bare ordinal; the second
    // flush's deposits run ahead of a root whose start the ordinals already
    // agree with.
    let mut rows = Rows::default();
    rows.flushed(0x01, 10, 7)
        .root(0x01, 10, 32, 2)
        .flushed(0x01, 10, 8)
        .flushed(0x01, 10, 9)
        .root(0x01, 10, 34, 4);

    let mut escrowed = escrow(7, usable_ciphertext());
    escrowed.extend(escrow(8, usable_ciphertext()));
    escrowed.extend(escrow(9, usable_ciphertext()));

    let plan = rows.plan(&escrowed).expect("tx is complete");

    assert_eq!(leaf_indices(&plan), (32..38).collect::<Vec<_>>());
    let cms: Vec<u8> = plan.notes.iter().map(|n| n.cm[0]).collect();
    assert_eq!(cms, [7, 7 ^ 0xff, 8, 8 ^ 0xff, 9, 9 ^ 0xff]);
}

#[test]
fn a_withdraw_with_interleaved_logs_keeps_the_next_item_aligned() {
    // A withdraw puts `AssetMoved` between its root and its notes, and the
    // token and adapter add logs of their own. None of them is a leaf, so
    // the transfer after it still starts where the withdraw ended.
    let mut rows = Rows::default();
    rows.nullifiers(0x01, 10, 0xa0)
        .root(0x01, 10, 40, 6)
        .foreign(0x01, 10)
        .asset_moved(0x01, 10)
        .notes(0x01, 10, 0xa0)
        .foreign(0x01, 10)
        .nullifiers(0x01, 10, 0xb0)
        .root(0x01, 10, 46, 6)
        .notes(0x01, 10, 0xb0);

    let plan = rows.plan_bare().expect("tx is complete");

    assert_eq!(leaf_indices(&plan), (40..52).collect::<Vec<_>>());
}

#[test]
fn a_swap_output_escrow_takes_no_leaf() {
    // The swap escrows its output after its notes. That deposit is inserted
    // by a later flush, so the item after the swap starts right after the
    // swap's six leaves.
    let mut rows = Rows::default();
    rows.nullifiers(0x01, 10, 0xa0)
        .root(0x01, 10, 0, 6)
        .asset_moved(0x01, 10)
        .notes(0x01, 10, 0xa0)
        .escrowed(0x01, 10, 9)
        .asset_moved(0x01, 10)
        .foreign(0x01, 10)
        .nullifiers(0x01, 10, 0xb0)
        .root(0x01, 10, 6, 6)
        .notes(0x01, 10, 0xb0);

    let plan = rows.plan_bare().expect("tx is complete");

    assert_eq!(leaf_indices(&plan), (0..12).collect::<Vec<_>>());
}

#[test]
fn a_bundle_that_stopped_early_commits_the_items_it_executed() {
    // The third item failed, so the tx holds only the first two items' logs
    // and the bundler's own. Nothing waits for a root that was never emitted,
    // and the next tx continues from where the prefix ended.
    let mut rows = Rows::default();
    rows.flushed(0x01, 10, 7)
        .root(0x01, 10, 0, 2)
        .nullifiers(0x01, 10, 0xa0)
        .root(0x01, 10, 2, 6)
        .notes(0x01, 10, 0xa0)
        .foreign(0x01, 10)
        .nullifiers(0x02, 11, 0xb0)
        .root(0x02, 11, 8, 6)
        .notes(0x02, 11, 0xb0);

    let plan = rows
        .plan(&escrow(7, usable_ciphertext()))
        .expect("both txs are complete");

    assert_eq!(leaf_indices(&plan), (0..14).collect::<Vec<_>>());
    assert_eq!(plan.last_block_number, 11);
}

/// The layout `Bundler.t.sol::test_execute_mixedBundle_logLayout` pins:
/// flush, transfer, withdraw, withdrawNative, swap, in one tx.
fn mixed_bundle() -> Rows {
    let (tx, b) = (0x01, 10);
    let mut rows = Rows::default();
    rows.flushed(tx, b, 7).root(tx, b, 0, 2);
    rows.nullifiers(tx, b, 0x10)
        .root(tx, b, 2, 6)
        .notes(tx, b, 0x10);
    rows.nullifiers(tx, b, 0x20)
        .root(tx, b, 8, 6)
        .asset_moved(tx, b)
        .notes(tx, b, 0x20);
    rows.nullifiers(tx, b, 0x30)
        .root(tx, b, 14, 6)
        .asset_moved(tx, b)
        .notes(tx, b, 0x30)
        .foreign(tx, b);
    rows.nullifiers(tx, b, 0x40)
        .root(tx, b, 20, 6)
        .asset_moved(tx, b)
        .notes(tx, b, 0x40)
        .escrowed(tx, b, 9)
        .asset_moved(tx, b)
        .foreign(tx, b);
    rows
}

#[test]
fn a_window_cut_between_bundle_items_commits_the_prefix() {
    let rows = mixed_bundle();
    let escrowed = escrow(7, usable_ciphertext());
    // Flush and transfer are whole; the withdraw is beyond the window.
    let cut = 13;

    let head = plan_window(&rows.0[..cut], AFTER, &escrowed)
        .expect("a window ending between items commits");
    assert_eq!(leaf_indices(&head), (0..8).collect::<Vec<_>>());
    assert_eq!(head.last_event_id, AFTER + cut as i64);

    // Next tick starts mid-tx and numbers it from the withdraw's own root.
    let tail =
        plan_window(&rows.0[cut..], head.last_event_id, &escrowed).expect("the rest commits");
    assert_eq!(leaf_indices(&tail), (8..26).collect::<Vec<_>>());

    // A cut inside an item waits instead: a deposit ahead of its root, or a
    // root ahead of its notes.
    assert!(plan_window(&rows.0[..1], AFTER, &escrowed).is_none());
    assert!(plan_window(&rows.0[..cut + 6], AFTER, &escrowed).is_none());
}

#[test]
fn a_window_cut_anywhere_in_a_bundle_commits_prefixes_that_add_up() {
    // Every window size, widened one row at a time as `plan_next` widens a
    // window that commits nothing. Whatever the cuts, the ticks together
    // must commit exactly what one tick over the whole tx does.
    let rows = mixed_bundle();
    let escrowed = escrow(7, usable_ciphertext());
    let whole = rows.plan(&escrowed).expect("the bundle is complete");
    assert_eq!(leaf_indices(&whole), (0..26).collect::<Vec<_>>());

    let len = rows.0.len();
    for window in 1..=len {
        let mut from = 0;
        let mut notes = Vec::new();
        let mut leaves = Vec::new();
        let mut nfs = 0;
        while from < len {
            let after = AFTER + from as i64;
            let mut to = (from + window).min(len);
            let plan = loop {
                if let Some(plan) = plan_window(&rows.0[from..to], after, &escrowed) {
                    break plan;
                }
                assert!(to < len, "window {window}: stuck at row {from}");
                to += 1;
            };
            notes.extend(leaf_indices(&plan));
            leaves.extend(tree_indices(&plan));
            nfs += plan.spent_nfs.len();
            from = (plan.last_event_id - AFTER) as usize;
        }
        assert_eq!(notes, leaf_indices(&whole), "window {window}");
        assert_eq!(leaves, notes, "window {window}");
        assert_eq!(nfs, whole.spent_nfs.len(), "window {window}");
    }
}
