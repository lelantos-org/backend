//! Behaviour of the per-chain tree mirror.

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

/// Two-leaf advance. Real spends insert `TRANSACT_OUT` leaves and a
/// flush one per deposit; two keeps the arithmetic in these tests easy
/// to read without changing what is under test.
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

/// A revert or a refused broadcast provably left no leaves on-chain, so
/// the speculative pair comes back off and the mirror stays usable.
#[test]
fn unwind_rolls_back_a_clean_failure() {
    let mut m = mirror(1);
    let before = m.current_root().unwrap();
    advance2(&mut m, cm(10), cm(11), cv(3), cv(4)).unwrap();

    let err = m.unwind(2, AppError::Reverted("tx reverted".into()));

    assert!(matches!(err, AppError::Reverted(_)));
    assert!(!m.is_desynced());
    assert_eq!(m.committed_count(), 2);
    assert_eq!(m.current_root().unwrap(), before, "root must be restored");
    reserve_one(&mut m).expect("mirror should still accept work");
}

/// The tx may still mine, so the leaves must stay — and because the mirror
/// can no longer be trusted either way, it stops accepting work.
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

/// Rolling back more leaves than exist cannot be honoured, so the mirror
/// parks rather than silently carrying on — but the caller still sees the
/// error that actually caused the unwind.
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

/// The corruption path: a wallet-supplied `cm` at or above the BN254
/// modulus makes Poseidon refuse the leaf. Before, leaf 0 was already in
/// the tree by then and nothing took it back out — the mirror silently
/// ran one leaf ahead of the chain forever.
#[test]
fn a_non_canonical_leaf_leaves_the_tree_untouched() {
    let mut m = mirror(1);
    let before_root = m.current_root().unwrap();
    let modulus: Field = crate::adapters::parse::BN254_R.to_be_bytes();

    let err = m
        .reserve_and_advance_batch(&[(cm(10), cv(3)), (modulus, cv(4))])
        .unwrap_err();

    assert!(matches!(err, AppError::Internal(_)), "got {err}");
    assert_eq!(m.committed_count(), 2, "no leaf may survive a failed batch");
    assert_eq!(m.current_root().unwrap(), before_root);
    assert!(!m.is_desynced(), "a rejected batch is not a desync");
    reserve_one(&mut m).expect("mirror should still accept work");
}

/// Same shape, but the bad element is the value commitment rather than the
/// commitment itself.
#[test]
fn a_non_canonical_cv_dep_also_leaves_the_tree_untouched() {
    let mut m = mirror(1);
    let bad = [*crate::adapters::parse::BN254_R, U256::from(1u8)];

    assert!(
        m.reserve_and_advance_batch(&[(cm(10), cv(3)), (cm(11), bad)])
            .is_err()
    );
    assert_eq!(m.committed_count(), 2);
}

/// Capacity is a length check, so it must come before hashing: an
/// oversized batch is refused without paying for a single Poseidon, which
/// is also what keeps this test instant.
#[test]
fn a_batch_past_capacity_is_refused_before_any_hashing() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    // Deliberately non-canonical: if the capacity check ran after hashing,
    // this would fail as a hash error instead — and would first hash a
    // million leaves to get there.
    let bad = *crate::adapters::parse::BN254_R;
    let leaves: Vec<(Field, [U256; 2])> = (0..MAX_LEAVES + 1)
        .map(|_| (bad.to_be_bytes::<32>(), cv(1)))
        .collect();

    let err = m.reserve_and_advance_batch(&leaves).unwrap_err();

    assert!(matches!(err, AppError::BadRequest(_)), "got {err}");
    assert!(err.to_string().contains("tree is full"), "got {err}");
    assert_eq!(m.committed_count(), 0);
}

/// `/chains` reads the snapshot, so it has to track every mutation — a
/// stale one would report a root the relayer is no longer building on.
#[test]
fn the_snapshot_tracks_every_mutation() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let snap = m.snapshot();
    assert_eq!(snap.leaf_count(), 0);
    assert!(!snap.is_desynced());

    advance2(&mut m, cm(1), cm(2), cv(1), cv(2)).unwrap();
    assert_eq!(snap.leaf_count(), 2);
    assert_eq!(snap.root(), m.current_root().unwrap());

    let _ = m.unwind(2, AppError::Reverted("nope".into()));
    assert_eq!(snap.leaf_count(), 0);
    assert_eq!(snap.root(), m.current_root().unwrap());

    let _ = m.unwind(0, AppError::SubmitUnknown("no receipt".into()));
    assert!(snap.is_desynced());
}

/// A payload naming a root this mirror has never held cannot land, so the
/// pipeline can reject it instead of proving against it.
#[test]
fn root_history_remembers_what_the_mirror_has_held() {
    let mut m = TreeMirror::new(CHAIN_ID).unwrap();
    let empty = m.current_root().unwrap();
    assert!(m.knows_root(&empty));

    advance2(&mut m, cm(1), cm(2), cv(1), cv(2)).unwrap();
    let after = m.current_root().unwrap();
    assert!(m.knows_root(&empty), "the previous root is still valid");
    assert!(m.knows_root(&after));
    assert!(!m.knows_root(&[0xEEu8; 32]));
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
    let first = m.current_root().unwrap();
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
