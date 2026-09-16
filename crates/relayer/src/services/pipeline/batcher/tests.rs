//! Queueing, ordering, size, anchor and outcome decoding, without a node.

use super::bundle::{drop_stale_roots, fits, gas_shares, reserve_item, take_bundle};
use super::outcome::{classify, decode_execute, decode_logs, error_name, execute_calldata};
use super::*;
use crate::services::tree::ROOT_HISTORY;
use alloy::primitives::{Bytes, keccak256};
use alloy::sol_types::SolCall;
use alloy::sol_types::SolEvent;
use std::collections::VecDeque;

fn encode_error(sig: &str, args: &[u8]) -> Vec<u8> {
    let mut out = keccak256(sig.as_bytes())[..4].to_vec();
    out.extend_from_slice(args);
    out
}

/// An item that is only ever queued, reserved and ordered; nothing proves or
/// encodes it. It inserts one leaf, keyed by its root, so reserving it advances
/// the tree.
struct Stub {
    entry: EntryPoint,
    root: Option<Field>,
}

impl BundleItem for Stub {
    fn entry(&self) -> EntryPoint {
        self.entry
    }

    fn leaves(&self) -> Vec<(Field, [U256; 2])> {
        vec![(
            self.root.unwrap_or_default(),
            [U256::from(1u8), U256::from(2u8)],
        )]
    }

    fn merkle_root(&self) -> Option<Field> {
        self.root
    }

    fn witness(&self, _: &ReservedSlot, _: &AdvancedState) -> TreeUpdateBatchWitness {
        unreachable!("stub items are never proved")
    }

    fn encode(
        &self,
        _: &ReservedSlot,
        _: &AdvancedState,
        _: IMasp::Proof,
    ) -> AppResult<IBundler::Call> {
        unreachable!("stub items are never encoded")
    }

    fn view(&self) -> QueuedItem {
        QueuedItem {
            kind: self.entry.as_str(),
            nullifiers: Vec::new(),
            deposit_ids: Vec::new(),
        }
    }
}

fn job(
    entry: EntryPoint,
    root: Option<Field>,
) -> (Job, oneshot::Receiver<AppResult<BundledReceipt>>) {
    let (reply, rx) = oneshot::channel();
    let job = Job {
        item: Box::new(Stub { entry, root }),
        reply,
        guard: None,
        proof: None,
    };
    (job, rx)
}

/// A tag to tell otherwise identical stubs apart.
fn tag(n: u8) -> Option<Field> {
    let mut f = [0u8; 32];
    f[31] = n;
    Some(f)
}

#[test]
fn a_bundle_takes_the_oldest_jobs_and_puts_swaps_last() {
    let mut pending: VecDeque<Job> = [
        (EntryPoint::Swap, tag(0)),
        (EntryPoint::Transfer, tag(1)),
        (EntryPoint::Flush, tag(2)),
        (EntryPoint::Swap, tag(3)),
        (EntryPoint::Withdraw, tag(4)),
    ]
    .into_iter()
    .map(|(entry, root)| job(entry, root).0)
    .collect();

    let bundle = take_bundle(&mut pending, 4);

    let order: Vec<Option<Field>> = bundle.iter().map(|j| j.item.merkle_root()).collect();
    assert_eq!(
        order,
        vec![tag(1), tag(2), tag(0), tag(3)],
        "queue order kept, swaps moved behind the rest"
    );
    assert_eq!(pending.len(), 1, "the fifth job waits for the next bundle");
    assert_eq!(pending[0].item.merkle_root(), tag(4));
}

#[test]
fn a_bundle_is_never_larger_than_the_queue() {
    let mut pending: VecDeque<Job> = VecDeque::from([job(EntryPoint::Transfer, None).0]);
    assert_eq!(take_bundle(&mut pending, 8).len(), 1);
    assert!(pending.is_empty());
}

fn call_with(data_len: usize) -> IBundler::Call {
    IBundler::Call {
        target: Address::ZERO,
        data: vec![0u8; data_len].into(),
    }
}

/// The size estimate is the exact ABI length of `execute`'s calldata.
#[test]
fn the_size_estimate_matches_the_encoded_calldata() {
    let calls = vec![call_with(100), call_with(33), call_with(0)];
    let encoded = execute_calldata(calls.clone()).len();
    assert_eq!(fits(&calls, encoded), None, "exactly at the cap fits");
    assert_eq!(fits(&calls, encoded - 1), Some(2));
}

#[test]
fn calls_past_the_cap_are_cut_and_an_oversized_first_call_fits_none() {
    let calls = vec![call_with(100), call_with(100), call_with(100)];
    // Head 68, then 4 words plus 128 bytes of data per call.
    assert_eq!(fits(&calls, 68 + 256 * 2), Some(2));
    assert_eq!(fits(&calls, 68 + 255), Some(0));
    assert_eq!(fits(&calls, usize::MAX), None);
}

fn advance(m: &mut TreeMirror, n: u8) {
    let mut cm = [0u8; 32];
    cm[31] = n;
    m.reserve_and_advance_batch(&[(cm, [U256::from(1u8), U256::from(2u8)])])
        .unwrap();
}

/// A bundle of `k` evicts `k` roots, so a root must be younger than
/// `ROOT_HISTORY - k` to still be accepted when its item lands.
#[test]
fn roots_that_would_expire_inside_the_bundle_are_refused() {
    let mut m = TreeMirror::new(31337).unwrap();
    let old = m.current_root();
    for n in 0..(ROOT_HISTORY - 3) as u8 {
        advance(&mut m, n);
    }
    assert_eq!(m.root_age(&old), Some(ROOT_HISTORY - 3));

    let (flush, _) = job(EntryPoint::Flush, None);
    let (spend, mut spend_rx) = job(EntryPoint::Transfer, Some(old));
    let mut jobs = vec![flush, spend];
    drop_stale_roots(&m, &mut jobs);
    assert_eq!(jobs.len(), 2, "age 61 in a bundle of 2 still lands");
    assert!(spend_rx.try_recv().is_err(), "not answered");

    let (spend_again, mut rx) = job(EntryPoint::Transfer, Some(old));
    let (unknown, mut unknown_rx) = job(EntryPoint::Withdraw, Some([0xEE; 32]));
    let mut jobs = vec![spend_again, unknown, job(EntryPoint::Flush, None).0];
    drop_stale_roots(&m, &mut jobs);
    assert_eq!(jobs.len(), 1, "age 61 in a bundle of 3 would expire");
    assert!(
        jobs[0].item.merkle_root().is_none(),
        "a flush proves no root"
    );
    for rx in [&mut rx, &mut unknown_rx] {
        assert!(matches!(rx.try_recv(), Ok(Err(AppError::BadRequest(_)))));
    }
}

/// Every item of a bundle names its anchor's slot as of the chain before the
/// bundle: the items reserved ahead of it write new slots, never that one.
#[test]
fn bundled_items_carry_the_anchor_slot_their_root_holds_on_chain() {
    let mut m = TreeMirror::new(31337).unwrap();
    // Past one lap of the ring, so slots and ages differ.
    let mut old = m.current_root();
    for n in 0..70u8 {
        advance(&mut m, n);
        if n == 49 {
            old = m.current_root();
        }
    }
    let current = m.current_root();
    let (current_slot, old_slot) = (
        m.anchor_index(&current).unwrap(),
        m.anchor_index(&old).unwrap(),
    );
    assert_eq!(current_slot, (70 % ROOT_HISTORY) as u8);
    assert_eq!(old_slot, (50 % ROOT_HISTORY) as u8);

    let items = [
        Stub {
            entry: EntryPoint::Flush,
            root: None,
        },
        Stub {
            entry: EntryPoint::Transfer,
            root: Some(old),
        },
        Stub {
            entry: EntryPoint::Withdraw,
            root: Some(current),
        },
        Stub {
            entry: EntryPoint::Swap,
            root: Some(old),
        },
    ];
    m.begin_bundle().unwrap();
    let anchors: Vec<Option<u8>> = items
        .iter()
        .map(|item| reserve_item(&mut m, item).unwrap().0.anchor_index)
        .collect();
    assert_eq!(
        anchors,
        vec![None, Some(old_slot), Some(current_slot), Some(old_slot)]
    );

    // Re-reserving the rest after a partial landing names the same slots.
    m.commit_prefix(1).unwrap();
    m.begin_bundle().unwrap();
    let again: Vec<Option<u8>> = items[1..]
        .iter()
        .map(|item| reserve_item(&mut m, item).unwrap().0.anchor_index)
        .collect();
    assert_eq!(again, anchors[1..]);

    // A root the mirror has never held is refused before anything is reserved.
    let leaves = m.committed_count();
    let unknown = Stub {
        entry: EntryPoint::Transfer,
        root: Some([0xEE; 32]),
    };
    assert!(matches!(
        reserve_item(&mut m, &unknown),
        Err(AppError::BadRequest(_))
    ));
    assert_eq!(m.committed_count(), leaves);
}

fn receipt_with(logs: Vec<alloy::rpc::types::Log>) -> SubmissionReceipt {
    SubmissionReceipt {
        tx_hash: Default::default(),
        block_number: 1,
        gas_used: 0,
        logs,
    }
}

fn log_from(address: Address, event: &impl SolEvent) -> alloy::rpc::types::Log {
    alloy::rpc::types::Log {
        inner: alloy::primitives::Log {
            address,
            data: event.encode_log_data(),
        },
        ..Default::default()
    }
}

#[test]
fn the_bundlers_events_say_how_far_a_bundle_got() {
    let bundler = Address::repeat_byte(0xb0);
    let reason = Bytes::from(encode_error("DoubleSpend()", &[]));
    let receipt = receipt_with(vec![
        // Another contract's lookalike event is ignored.
        log_from(
            Address::repeat_byte(0x01),
            &IBundler::BundleExecuted {
                executed: U256::ZERO,
                total: U256::from(3),
            },
        ),
        log_from(
            bundler,
            &IBundler::BundleItemFailed {
                index: U256::from(1),
                reason: reason.clone(),
            },
        ),
        log_from(
            bundler,
            &IBundler::BundleExecuted {
                executed: U256::from(1),
                total: U256::from(3),
            },
        ),
    ]);
    assert_eq!(decode_logs(&receipt, bundler, 3), (Some(1), Some(reason)));
}

/// `execute` emits `BundleExecuted` whenever it returns, so a receipt without
/// one says nothing about how far the bundle got.
#[test]
fn a_receipt_without_bundler_events_is_an_unknown_outcome() {
    let bundler = Address::repeat_byte(0xb0);
    assert_eq!(
        decode_logs(&receipt_with(Vec::new()), bundler, 4),
        (None, None)
    );
}

#[test]
fn the_mirror_finds_the_bundle_prefix_a_chain_root_marks() {
    let mut m = TreeMirror::new(1).unwrap();
    let base = m.current_root();
    m.begin_bundle().unwrap();
    let flush = Stub {
        entry: EntryPoint::Flush,
        root: None,
    };
    let (_, first) = reserve_item(&mut m, &flush).unwrap();
    let (_, second) = reserve_item(&mut m, &flush).unwrap();
    assert_eq!(m.bundle_prefix_reaching(&first.new_root), Some(1));
    assert_eq!(m.bundle_prefix_reaching(&second.new_root), Some(2));
    assert_eq!(
        m.bundle_prefix_reaching(&base),
        None,
        "no items reach the base root"
    );
    assert_eq!(m.bundle_prefix_reaching(&[0xEE; 32]), None);
}

#[test]
fn gas_shares_split_proportionally_and_sum_exactly() {
    let shares = gas_shares(1_000_001, &[500_000, 500_000, 1_000_000]);
    assert_eq!(shares.iter().sum::<u64>(), 1_000_001);
    assert_eq!(shares[0], 250_000);
    assert_eq!(shares[1], 250_000);
    assert_eq!(
        shares[2], 500_001,
        "the rounding remainder lands on the last"
    );
}

#[test]
fn a_zero_weight_still_gets_a_share() {
    let shares = gas_shares(300, &[0, 0, 0]);
    assert_eq!(shares, vec![100, 100, 100]);
}

#[test]
fn a_stale_root_is_recognised_as_the_chain_moving() {
    for sig in ["StaleOldRoot()", "BatchMisaligned()"] {
        let f = classify(&encode_error(sig, &[]));
        assert!(f.stale_root, "{sig}");
    }
    assert!(!classify(&encode_error("DoubleSpend()", &[])).stale_root);
}

#[test]
fn a_known_error_becomes_a_contract_rejection_naming_it() {
    let f = classify(&encode_error(
        "InsufficientOut(uint256,uint256)",
        &[0u8; 64],
    ));
    match f.error {
        AppError::ContractRejected { reason, .. } => assert_eq!(reason, "InsufficientOut"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_revert_string_is_decoded() {
    let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
    data.extend(<String as alloy::sol_types::SolValue>::abi_encode(
        &"MockSwapAdapter: insufficient out".to_string(),
    ));
    assert_eq!(
        error_name(&data).as_deref(),
        Some("MockSwapAdapter: insufficient out")
    );
}

#[test]
fn an_unknown_revert_is_reported_raw() {
    let f = classify(&[0xde, 0xad, 0xbe, 0xef]);
    assert!(!f.stale_root);
    assert!(matches!(f.error, AppError::Reverted(_)));
}

#[test]
fn execute_return_data_reports_the_first_failing_item() {
    let out = IBundler::executeCall::abi_encode_returns(&(
        U256::from(2),
        Bytes::from(encode_error("DoubleSpend()", &[])),
    ));
    let f = decode_execute(&out, 5).expect("stopped");
    assert_eq!(f.index, 2);
    assert_eq!(error_name(&f.reason).as_deref(), Some("DoubleSpend"));
    assert!(decode_execute(&out, 2).is_none(), "all of 2 executed");
}
