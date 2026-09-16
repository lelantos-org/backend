//! Behaviour of the per-chain tree mirror.

use super::bootstrap::BootSource;
use super::*;

const CHAIN_ID: i64 = 31337;

fn cm(n: u8) -> Field {
    let mut f = [0u8; 32];
    f[31] = n;
    f
}

fn cv(n: u8) -> [U256; 2] {
    [U256::from(n), U256::from(n) + U256::from(1u8)]
}

/// Two-leaf advance. Real spends insert `TRANSACT_OUT` leaves and a flush inserts
/// one per deposit; two keeps the arithmetic in these tests simple without
/// changing what is under test.
fn advance2(
    m: &mut TreeMirror,
    cm0: Field,
    cm1: Field,
    cv0: [U256; 2],
    cv1: [U256; 2],
) -> AppResult<(ReservedSlot, AdvancedState)> {
    m.reserve_and_advance_batch(&[(cm0, cv0), (cm1, cv1)])
}

/// A mirror with `pairs` pairs already committed.
fn mirror(pairs: u8) -> TreeMirror {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    for i in 0..pairs {
        advance2(&mut m, cm(2 * i), cm(2 * i + 1), cv(i), cv(i + 1)).unwrap();
    }
    m
}

fn reserve_one(m: &mut TreeMirror) -> AppResult<()> {
    advance2(m, cm(200), cm(201), cv(9), cv(10)).map(|_| ())
}

#[test]
fn reserve_advances_by_two_leaves() {
    let mut m = mirror(1);
    assert_eq!(m.committed_count(), 2);
    let (slot, advanced) = advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();
    assert_eq!(slot.start_index, 2);
    assert_eq!(m.committed_count(), 4);
    assert_ne!(slot.old_root, advanced.new_root);
}

/// A revert or a refused broadcast provably left no leaves on chain, so the
/// speculative pair is removed and the mirror stays usable.
#[test]
fn unwind_rolls_back_a_clean_failure() {
    let mut m = mirror(1);
    let before = m.current_root();
    advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();

    let err = m.unwind(2, AppError::Reverted("tx reverted".into()));

    assert!(matches!(err, AppError::Reverted(_)));
    assert!(!m.is_desynced());
    assert_eq!(m.committed_count(), 2);
    assert_eq!(m.current_root(), before, "root must be restored");
    reserve_one(&mut m).expect("mirror should still accept work");
}

/// The transaction may still mine, so the leaves stay, and the mirror stops
/// accepting work because it cannot be trusted either way.
#[test]
fn unwind_parks_on_an_unknown_outcome() {
    let mut m = mirror(1);
    advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();

    let err = m.unwind(2, AppError::SubmitUnknown("no receipt".into()));

    assert!(matches!(err, AppError::SubmitUnknown(_)));
    assert!(m.is_desynced());
    assert_eq!(m.committed_count(), 4, "speculative leaves must be kept");
    assert!(matches!(
        reserve_one(&mut m),
        Err(AppError::MirrorDesynced(_))
    ));
}

/// Rolling back more leaves than exist cannot be honoured, so the mirror parks
/// rather than continuing, while the caller still sees the error that caused the
/// unwind.
#[test]
fn unwind_parks_when_the_rollback_itself_fails() {
    let mut m = mirror(1);

    let err = m.unwind(99, AppError::Reverted("tx reverted".into()));

    assert!(matches!(err, AppError::Reverted(_)));
    assert!(m.is_desynced());
    assert!(matches!(
        reserve_one(&mut m),
        Err(AppError::MirrorDesynced(_))
    ));
}

#[test]
fn parking_keeps_the_first_reason() {
    let mut m = mirror(1);
    let _ = m.unwind(2, AppError::SubmitUnknown("first".into()));
    let _ = m.unwind(2, AppError::SubmitUnknown("second".into()));

    let Err(AppError::MirrorDesynced(reason)) = reserve_one(&mut m) else {
        panic!("expected a desynced mirror");
    };
    assert!(reason.contains("first"), "got {reason}");
}

/// A wallet-supplied `cm` at or above the BN254 modulus makes Poseidon refuse the
/// leaf. Hashing after the first insert would leave leaf 0 in the tree with
/// nothing to remove it, running the mirror one leaf ahead of the chain.
#[test]
fn a_non_canonical_leaf_leaves_the_tree_untouched() {
    let mut m = mirror(1);
    let before_root = m.current_root();
    let modulus: Field = crate::domain::field::BN254_R.to_be_bytes();

    let err = m
        .reserve_and_advance_batch(&[(cm(10), cv(3)), (modulus, cv(4))])
        .unwrap_err();

    assert!(matches!(err, AppError::Internal(_)), "got {err}");
    assert_eq!(m.committed_count(), 2, "no leaf may survive a failed batch");
    assert_eq!(m.current_root(), before_root);
    assert!(!m.is_desynced(), "a rejected batch is not a desync");
    reserve_one(&mut m).expect("mirror should still accept work");
}

/// Same shape, but the bad element is the value commitment rather than the
/// commitment itself.
#[test]
fn a_non_canonical_cv_dep_also_leaves_the_tree_untouched() {
    let mut m = mirror(1);
    let bad = [*crate::domain::field::BN254_R, U256::from(1u8)];

    assert!(
        m.reserve_and_advance_batch(&[(cm(10), cv(3)), (cm(11), bad)])
            .is_err()
    );
    assert_eq!(m.committed_count(), 2);
}

/// Capacity is a length check and must precede hashing: an oversized batch is
/// refused without computing a single Poseidon, which also keeps this test fast.
#[test]
fn a_batch_past_capacity_is_refused_before_any_hashing() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    // Non-canonical on purpose: if the capacity check ran after hashing, this
    // would fail as a hash error, after hashing a million leaves.
    let bad = *crate::domain::field::BN254_R;
    let leaves: Vec<(Field, [U256; 2])> = (0..MAX_LEAVES + 1)
        .map(|_| (bad.to_be_bytes::<32>(), cv(1)))
        .collect();

    let err = m.reserve_and_advance_batch(&leaves).unwrap_err();

    assert!(matches!(err, AppError::BadRequest(_)), "got {err}");
    assert!(err.to_string().contains("tree is full"), "got {err}");
    assert_eq!(m.committed_count(), 0);
}

/// `/chains` reads the snapshot, so it must track every mutation; a stale one
/// would report a root the relayer has stopped building on.
#[test]
fn the_snapshot_tracks_every_mutation() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let snap = m.snapshot();
    assert_eq!(snap.leaf_count(), 0);
    assert!(!snap.is_desynced());

    advance2(&mut m, cm(1), cm(2), cv(1), cv(2)).unwrap();
    assert_eq!(snap.leaf_count(), 2);
    assert_eq!(snap.root(), m.current_root());

    let _ = m.unwind(2, AppError::Reverted("nope".into()));
    assert_eq!(snap.leaf_count(), 0);
    assert_eq!(snap.root(), m.current_root());

    let _ = m.unwind(0, AppError::SubmitUnknown("no receipt".into()));
    assert!(snap.is_desynced());
}

/// A payload naming a root this mirror has never held cannot land, so the pipeline
/// rejects it rather than proving against it.
#[test]
fn root_history_remembers_what_the_mirror_has_held() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let empty = m.current_root();
    assert!(m.knows_root(&empty));

    advance2(&mut m, cm(1), cm(2), cv(1), cv(2)).unwrap();
    let after = m.current_root();
    assert!(m.knows_root(&empty), "the previous root is still valid");
    assert!(m.knows_root(&after));
    assert!(!m.knows_root(&[0xEEu8; 32]));
}

/// A rolled-back advance published a root the chain never held. Leaving it in the
/// accepted window would let a wallet that read it from `/chains` pass the
/// batcher's root check and then revert `UnknownRoot` on chain.
#[test]
fn a_rolled_back_root_stops_being_accepted() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let before = m.current_root();

    advance2(&mut m, cm(1), cm(2), cv(1), cv(2)).unwrap();
    let speculative = m.current_root();
    assert!(m.knows_root(&speculative), "published while in flight");

    m.rollback(2).unwrap();
    assert!(
        !m.knows_root(&speculative),
        "retracted root is still accepted"
    );
    assert!(
        m.knows_root(&before),
        "the root it reverted to is still valid"
    );
    assert_eq!(m.current_root(), before);
}

/// Only the newest entry is retracted. An identical root deeper in the window was
/// reached by an advance that landed and stays valid.
#[test]
fn a_rollback_retracts_only_the_advance_it_undid() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    advance2(&mut m, cm(1), cm(2), cv(1), cv(2)).unwrap();
    let landed = m.current_root();

    advance2(&mut m, cm(3), cm(4), cv(3), cv(4)).unwrap();
    m.rollback(2).unwrap();

    assert!(m.knows_root(&landed));
    assert_eq!(m.current_root(), landed);
}

/// A rollback restores an earlier root, which must not be pushed twice.
#[test]
fn an_unchanged_root_does_not_consume_a_slot() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let before = m.recent_roots.len();
    m.publish();
    m.publish();
    assert_eq!(m.recent_roots.len(), before);
}

#[test]
fn root_history_is_bounded() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let first = m.current_root();
    for i in 0..ROOT_HISTORY as u8 + 2 {
        advance2(&mut m, cm(i), cm(i + 1), cv(i), cv(i + 1)).unwrap();
    }
    assert!(m.recent_roots.len() <= ROOT_HISTORY);
    assert!(!m.knows_root(&first), "the oldest root must roll off");
}

#[test]
fn rollback_past_the_start_is_rejected() {
    let mut m = mirror(1);
    assert!(m.rollback(3).is_err());
    assert_eq!(
        m.committed_count(),
        2,
        "a rejected rollback changes nothing"
    );
}

/// The frontier a rollback restores is what the next proof is built against, so
/// re-reserving after an undone advance must hand back the same starting state,
/// not just the same root.
#[test]
fn a_rollback_restores_the_state_the_next_proof_builds_on() {
    let mut m = mirror(2);
    let (first, _) = advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();
    m.rollback(2).unwrap();

    let (again, _) = advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();
    assert_eq!(again.start_index, first.start_index);
    assert_eq!(again.old_root, first.old_root);
    assert_eq!(again.old_frontier, first.old_frontier);
}

/// The mirror can only return to its last reserve: a frontier keeps no record of
/// what it folded, so a count naming any other state is refused rather than
/// silently landing somewhere neither the mirror nor the chain has been.
#[test]
fn rollback_of_anything_but_the_last_reserve_is_rejected() {
    let mut m = mirror(2);
    advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();
    let root = m.current_root();

    assert!(m.rollback(1).is_err(), "half a batch is not a state");
    assert!(m.rollback(4).is_err(), "an earlier reserve is gone");
    assert_eq!(
        m.committed_count(),
        6,
        "a rejected rollback changes nothing"
    );
    assert_eq!(m.current_root(), root);

    m.rollback(2).expect("the last reserve is still undoable");
    assert_eq!(m.committed_count(), 4);
}

// -- bundles ------------------------------------------------------------------

/// Three items chained in one bundle and all kept: the mirror ends where a
/// landed bundle leaves the chain, and every intermediate root stays accepted,
/// since the pool registers each item's root.
#[test]
fn a_fully_kept_bundle_keeps_every_item_and_root() {
    let mut m = mirror(1);
    m.begin_bundle().unwrap();
    let (a, a_adv) = advance2(&mut m, cm(10), cm(11), cv(1), cv(2)).unwrap();
    let (b, b_adv) = advance2(&mut m, cm(12), cm(13), cv(3), cv(4)).unwrap();
    let (c, c_adv) = advance2(&mut m, cm(14), cm(15), cv(5), cv(6)).unwrap();

    assert_eq!(b.start_index, a.start_index + 2, "items chain");
    assert_eq!(b.old_root, a_adv.new_root, "each builds on the one before");
    assert_eq!(c.old_root, b_adv.new_root);
    assert_eq!(m.bundle_len(), 3);

    m.commit_prefix(3).unwrap();
    assert_eq!(m.bundle_len(), 0, "the bundle is closed");
    assert_eq!(m.committed_count(), 8);
    assert_eq!(m.current_root(), c_adv.new_root);
    for root in [a_adv.new_root, b_adv.new_root, c_adv.new_root] {
        assert!(m.knows_root(&root), "every landed root is accepted");
    }
}

/// The chain stopped after the first item: the mirror keeps exactly that item,
/// and the discarded items' roots leave the accepted window.
#[test]
fn a_partial_prefix_keeps_only_the_items_that_landed() {
    let mut m = mirror(1);
    m.begin_bundle().unwrap();
    let (_, a_adv) = advance2(&mut m, cm(10), cm(11), cv(1), cv(2)).unwrap();
    let (b, b_adv) = advance2(&mut m, cm(12), cm(13), cv(3), cv(4)).unwrap();
    let (_, c_adv) = advance2(&mut m, cm(14), cm(15), cv(5), cv(6)).unwrap();

    m.commit_prefix(1).unwrap();
    assert_eq!(m.committed_count(), 4, "one item of two leaves kept");
    assert_eq!(m.current_root(), a_adv.new_root);
    assert!(m.knows_root(&a_adv.new_root));
    assert!(!m.knows_root(&b_adv.new_root), "discarded root retracted");
    assert!(!m.knows_root(&c_adv.new_root), "discarded root retracted");

    // Re-reserving the discarded item lands it exactly where it was, so a proof
    // made for it before the rollback is still the right one.
    let (again, again_adv) = advance2(&mut m, cm(12), cm(13), cv(3), cv(4)).unwrap();
    assert_eq!(again.start_index, b.start_index);
    assert_eq!(again.old_root, b.old_root);
    assert_eq!(again.old_frontier, b.old_frontier);
    assert_eq!(again_adv.new_root, b_adv.new_root);
}

#[test]
fn rolling_back_a_bundle_restores_the_pre_bundle_state() {
    let mut m = mirror(2);
    let root = m.current_root();
    let window = m.recent_roots.clone();

    m.begin_bundle().unwrap();
    advance2(&mut m, cm(10), cm(11), cv(1), cv(2)).unwrap();
    advance2(&mut m, cm(12), cm(13), cv(3), cv(4)).unwrap();
    m.rollback_bundle().unwrap();

    assert_eq!(m.committed_count(), 4);
    assert_eq!(m.current_root(), root);
    assert_eq!(m.recent_roots, window, "accepted window as it was");
}

#[test]
fn a_bundle_cannot_be_opened_twice_or_kept_past_its_length() {
    let mut m = mirror(1);
    m.begin_bundle().unwrap();
    assert!(m.begin_bundle().is_err(), "already open");
    advance2(&mut m, cm(10), cm(11), cv(1), cv(2)).unwrap();
    assert!(m.commit_prefix(2).is_err(), "only one item reserved");
    assert!(
        m.commit_prefix(0).is_err(),
        "the failed commit closed the bundle"
    );
}

/// An unknown outcome may still land, so the bundle's leaves stay and the mirror
/// parks, exactly as a single batch does.
#[test]
fn abandoning_a_bundle_on_an_unknown_outcome_parks() {
    let mut m = mirror(1);
    m.begin_bundle().unwrap();
    advance2(&mut m, cm(10), cm(11), cv(1), cv(2)).unwrap();

    let err = m.abandon_bundle(AppError::SubmitUnknown("no receipt".into()));
    assert!(matches!(err, AppError::SubmitUnknown(_)));
    assert!(m.is_desynced());
    assert_eq!(m.committed_count(), 4, "leaves kept");
    assert!(
        m.begin_bundle().is_err(),
        "parked mirror refuses new bundles"
    );
}

#[test]
fn abandoning_a_bundle_on_a_clean_failure_rolls_it_back() {
    let mut m = mirror(1);
    let root = m.current_root();
    m.begin_bundle().unwrap();
    advance2(&mut m, cm(10), cm(11), cv(1), cv(2)).unwrap();

    let err = m.abandon_bundle(AppError::Rpc("refused".into()));
    assert!(matches!(err, AppError::Rpc(_)));
    assert!(!m.is_desynced());
    assert_eq!(m.current_root(), root);
}

#[test]
fn root_age_counts_advances_since_a_root_was_current() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let empty = m.current_root();
    assert_eq!(m.root_age(&empty), Some(0));
    advance2(&mut m, cm(1), cm(2), cv(1), cv(2)).unwrap();
    advance2(&mut m, cm(3), cm(4), cv(3), cv(4)).unwrap();
    assert_eq!(m.root_age(&empty), Some(2));
    assert_eq!(m.root_age(&m.current_root()), Some(0));
    assert_eq!(m.root_age(&[0xEEu8; 32]), None);
}

/// `BatchAppend` pins every frontier slot a digit does not read to zero, so the
/// witness a bundle item is proved against must carry zeros there even after the
/// mirror has been rolled back and re-reserved.
#[test]
fn a_bundle_item_witness_zeroes_the_frontier_slots_it_does_not_read() {
    let mut m = mirror(1);
    m.begin_bundle().unwrap();
    advance2(&mut m, cm(10), cm(11), cv(1), cv(2)).unwrap();
    m.rollback_bundle().unwrap();
    m.begin_bundle().unwrap();
    advance2(&mut m, cm(12), cm(13), cv(3), cv(4)).unwrap();
    let (slot, _) = m.reserve_and_advance_batch(&[(cm(14), cv(5))]).unwrap();
    // Digit d of the start index is how many slots level d reads.
    for (level, row) in slot.old_frontier.iter().enumerate() {
        let digit = ((slot.start_index >> (2 * level)) & 3) as usize;
        for (k, value) in row.iter().enumerate().skip(digit) {
            assert_eq!(*value, [0u8; 32], "level {level} slot {k} is not read");
        }
    }
}

// -- anchor slots ---------------------------------------------------------------

/// `CommitmentTree`'s root ring, transcribed: genesis seeds slot 0, every advance
/// writes the next slot mod 64, and `rootIndexOf` scans newest first.
struct Ring {
    roots: [Field; ROOT_HISTORY],
    index: usize,
}

impl Ring {
    fn genesis() -> Self {
        let mut roots = [[0u8; 32]; ROOT_HISTORY];
        roots[0] = empty_root().unwrap();
        Self { roots, index: 0 }
    }

    fn advance(&mut self, new_root: Field) {
        self.index = (self.index + 1) % ROOT_HISTORY;
        self.roots[self.index] = new_root;
    }

    fn root_index_of(&self, root: &Field) -> Option<u8> {
        let mut idx = self.index;
        for _ in 0..ROOT_HISTORY {
            if self.roots[idx] == *root {
                return Some(idx as u8);
            }
            idx = (idx + ROOT_HISTORY - 1) % ROOT_HISTORY;
        }
        None
    }
}

/// One advance on both the mirror and the ring. Leaves are unique per `n`, so
/// every root differs.
fn advance_both(m: &mut TreeMirror, ring: &mut Ring, n: u16) {
    let (_, advanced) = advance_n(m, n);
    ring.advance(advanced.new_root);
}

fn advance_n(m: &mut TreeMirror, n: u16) -> (ReservedSlot, AdvancedState) {
    let mut c = [0u8; 32];
    c[30..].copy_from_slice(&n.to_be_bytes());
    m.reserve_and_advance_batch(&[(c, cv(1))]).unwrap()
}

/// Every root the mirror accepts resolves to the slot the pool holds it in, and
/// the pool holds nothing the mirror would not accept.
fn assert_anchors_match(m: &TreeMirror, ring: &Ring) {
    assert_anchors_agree(m, ring);
    for root in ring.roots.iter().filter(|r| **r != [0u8; 32]) {
        assert!(
            m.knows_root(root),
            "the pool holds a root the mirror dropped"
        );
    }
}

/// Every root the mirror accepts resolves to the slot the pool holds it in. The
/// mirror may accept fewer: a single-batch rollback does not bring back the root
/// its advance evicted, which only narrows the window.
fn assert_anchors_agree(m: &TreeMirror, ring: &Ring) {
    assert_eq!(m.ring_index, Some(ring.index), "ring position");
    for root in &m.recent_roots {
        assert_eq!(
            m.anchor_index(root).ok(),
            ring.root_index_of(root),
            "anchor of {}",
            field_to_hex(root)
        );
    }
}

/// The mirror's empty tree is the pool's genesis root, so a fresh mirror's slot 0
/// is the pool's slot 0.
#[test]
fn the_empty_root_is_the_pools_genesis_root() {
    assert_eq!(
        field_to_hex(&empty_root().unwrap()),
        "0x1cf92e62b512433b35f0064d537576b0184cad5fa7ab64201cd8084ee2dc171f",
        "CommitmentTree.EMPTY_ROOT"
    );
    let m = TreeMirror::new(CHAIN_ID).unwrap();
    assert_eq!(m.anchor_index(&m.current_root()).unwrap(), 0);
}

/// Past 64 advances the ring overwrites its oldest slot; the mirror's slots follow
/// it all the way round, twice.
#[test]
fn anchor_slots_follow_the_pools_ring_through_wrap_around() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let mut ring = Ring::genesis();
    assert_anchors_match(&m, &ring);
    for n in 0..(2 * ROOT_HISTORY as u16 + 5) {
        advance_both(&mut m, &mut ring, n);
        assert_anchors_match(&m, &ring);
    }
    assert!(
        m.anchor_index(&empty_root().unwrap()).is_err(),
        "the overwritten genesis root has no slot"
    );
}

/// Speculative advances inside a bundle do not move an existing root's slot, so
/// every item anchored on the same root names the same slot, whichever position
/// it holds in the bundle.
#[test]
fn a_bundles_items_see_the_same_anchor_slot() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let mut ring = Ring::genesis();
    for n in 0..70 {
        advance_both(&mut m, &mut ring, n);
    }
    let old = m.recent_roots[m.recent_roots.len() - 20];
    let current = m.current_root();
    let (old_slot, current_slot) = (ring.root_index_of(&old), ring.root_index_of(&current));

    m.begin_bundle().unwrap();
    let mut landed = Ring { ..ring };
    for n in 100..105 {
        assert_eq!(m.anchor_index(&old).ok(), old_slot);
        assert_eq!(m.anchor_index(&current).ok(), current_slot);
        advance_both(&mut m, &mut landed, n);
    }
    // The roots the bundle produces land where the pool would put them.
    assert_anchors_match(&m, &landed);
    m.commit_prefix(5).unwrap();
    assert_anchors_match(&m, &landed);
}

/// Keeping a prefix restores the ring position the chain actually reached, so the
/// items after it and the next bundle anchor as the pool does.
#[test]
fn committing_a_prefix_keeps_the_ring_position_of_the_items_that_landed() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let mut ring = Ring::genesis();
    for n in 0..62 {
        advance_both(&mut m, &mut ring, n);
    }
    m.begin_bundle().unwrap();
    // Kept items cross the 64-slot boundary; the discarded ones would too.
    let mut roots = Vec::new();
    for n in 100..106 {
        roots.push(advance_n(&mut m, n).1.new_root);
    }
    m.commit_prefix(3).unwrap();
    for root in &roots[..3] {
        ring.advance(*root);
    }
    assert_anchors_match(&m, &ring);
    for root in &roots[3..] {
        assert!(
            m.anchor_index(root).is_err(),
            "a discarded root has no slot"
        );
    }

    for n in 200..203 {
        advance_both(&mut m, &mut ring, n);
        assert_anchors_match(&m, &ring);
    }
}

#[test]
fn rolling_back_restores_the_ring_position() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let mut ring = Ring::genesis();
    for n in 0..66 {
        advance_both(&mut m, &mut ring, n);
    }

    // A single reserve, undone. Its advance evicted the oldest root, which the
    // rollback does not bring back.
    advance_n(&mut m, 500);
    m.rollback(1).unwrap();
    assert_anchors_agree(&m, &ring);
    assert_eq!(m.recent_roots.len(), ROOT_HISTORY - 1);

    // A whole bundle, undone.
    m.begin_bundle().unwrap();
    for n in 600..604 {
        advance_n(&mut m, n);
    }
    m.rollback_bundle().unwrap();
    assert_anchors_agree(&m, &ring);

    // A bundle abandoned on a clean failure.
    m.begin_bundle().unwrap();
    advance_n(&mut m, 700);
    let _ = m.abandon_bundle(AppError::Rpc("refused".into()));
    assert_anchors_agree(&m, &ring);

    for n in 800..802 {
        advance_both(&mut m, &mut ring, n);
        assert_anchors_agree(&m, &ring);
    }
}

/// The indexer's history, newest first, as `tree_advances::recent_roots` returns
/// it: the last `ROOT_HISTORY` roots `ring` accepted, given `advances` of them.
fn history_of(ring: &Ring, advances: usize) -> Vec<Vec<u8>> {
    (0..advances.min(ROOT_HISTORY))
        .map(|age| ring.roots[(ring.index + ROOT_HISTORY - age) % ROOT_HISTORY].to_vec())
        .collect()
}

/// Mirror `advances` advances on a fresh mirror, then scramble its window, so only
/// a rebuild from the history can bring it back.
fn scrambled(advances: u16) -> (TreeMirror, Ring) {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let mut ring = Ring::genesis();
    for n in 0..advances {
        advance_both(&mut m, &mut ring, n);
    }
    m.recent_roots.push_front([0xAA; 32]);
    m.recent_roots.rotate_left(1);
    m.ring_index = Some(13);
    (m, ring)
}

/// A young chain's history is all of it, so the window regains the genesis root
/// at slot 0, and the pool's `rootIndex` must agree with the history's length.
#[test]
fn bootstrapping_a_young_chain_keeps_the_genesis_root_in_slot_zero() {
    let (mut m, ring) = scrambled(5);
    m.adopt_history(&history_of(&ring, 5), BootSource::TreeState)
        .unwrap();
    assert!(
        m.anchor_index(&m.current_root()).is_err(),
        "no anchor before the pool's ring position is read"
    );
    assert!(m.adopt_ring_index(4).is_err(), "history misses an advance");
    assert!(m.adopt_ring_index(64).is_err(), "not a ring slot");
    m.adopt_ring_index(5).unwrap();
    assert_anchors_match(&m, &ring);
    assert_eq!(m.anchor_index(&empty_root().unwrap()).unwrap(), 0);
}

#[test]
fn bootstrapping_a_fresh_chain_anchors_on_the_genesis_root() {
    let (mut m, ring) = scrambled(0);
    m.adopt_history(&[], BootSource::Notes).unwrap();
    m.adopt_ring_index(0).unwrap();
    assert_anchors_match(&m, &ring);
}

/// Past 64 advances the history is exactly the ring, and the position can only
/// come from the pool.
#[test]
fn bootstrapping_a_wrapped_chain_adopts_the_pools_ring_position() {
    let (mut m, ring) = scrambled(100);
    m.adopt_history(&history_of(&ring, 100), BootSource::TreeState)
        .unwrap();
    m.adopt_ring_index(ring.index as u32).unwrap();
    assert_eq!(ring.index, 100 % ROOT_HISTORY);
    assert_anchors_match(&m, &ring);
}

/// 63 advances fill the ring but for genesis; 64 overwrite it. Either side of the
/// edge the window is the ring.
#[test]
fn bootstrapping_at_the_edge_of_a_full_ring() {
    for advances in [63u16, 64, 65] {
        let (mut m, ring) = scrambled(advances);
        m.adopt_history(&history_of(&ring, advances as usize), BootSource::TreeState)
            .unwrap();
        assert_eq!(m.recent_roots.len(), ROOT_HISTORY, "{advances}");
        m.adopt_ring_index(ring.index as u32).unwrap();
        assert_anchors_match(&m, &ring);
    }
}

/// The chain moved without this mirror: a resync rebuilds from the indexer's
/// history over the chain's tree, and the next advances anchor as the pool does.
#[test]
fn a_resync_after_foreign_advances_anchors_as_the_pool_does() {
    let mut ours = TreeMirror::new(CHAIN_ID).unwrap();
    let mut chain = TreeMirror::new(CHAIN_ID).unwrap();
    let mut ring = Ring::genesis();
    for n in 0..10 {
        advance_n(&mut ours, n);
        advance_both(&mut chain, &mut ring, n);
    }
    // Another relayer lands 60 advances.
    for n in 1000..1060 {
        advance_both(&mut chain, &mut ring, n);
    }

    ours.tree = chain.tree.clone();
    ours.adopt_history(&history_of(&ring, 70), BootSource::TreeState)
        .unwrap();
    ours.adopt_ring_index(ring.index as u32).unwrap();
    assert_anchors_match(&ours, &ring);

    for n in 2000..2004 {
        advance_both(&mut ours, &mut ring, n);
        assert_anchors_match(&ours, &ring);
    }
}

/// A history whose newest root is not the tree's is a divergence, as before.
#[test]
fn a_history_that_disagrees_with_the_tree_is_refused() {
    let (mut m, ring) = scrambled(3);
    let mut history = history_of(&ring, 3);
    history[0] = vec![0xEE; 32];
    assert!(m.adopt_history(&history, BootSource::TreeState).is_err());
    // An empty history over a non-empty tree too.
    assert!(m.adopt_history(&[], BootSource::Notes).is_err());
}
