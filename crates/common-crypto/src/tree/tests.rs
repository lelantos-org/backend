use super::hash::hash_node;
use super::*;

fn leaf(n: u64) -> Field {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&n.to_be_bytes());
    out
}

#[test]
fn empty_tree_root_matches_iterated_zero() {
    let t = MerkleTree::new(2).expect("new");
    let r = t.root().expect("root");
    // Manually fold: lvl1 = Poseidon(5, 0,0,0,0); root = Poseidon(5, lvl1×4)
    let z = [0u8; 32];
    let lvl1 = hash_node(&[z, z, z, z]).unwrap();
    let expected = hash_node(&[lvl1, lvl1, lvl1, lvl1]).unwrap();
    assert_eq!(r, expected);
}

#[test]
fn single_leaf_root_matches_direct() {
    let mut t = MerkleTree::new(2).expect("new");
    let l = leaf(0xabcd);
    t.insert(l).unwrap();
    let z = [0u8; 32];
    let bottom0 = hash_node(&[l, z, z, z]).unwrap();
    let bottom_z = hash_node(&[z, z, z, z]).unwrap();
    let expected = hash_node(&[bottom0, bottom_z, bottom_z, bottom_z]).unwrap();
    assert_eq!(t.root().unwrap(), expected);
}

#[test]
fn frontier_zero_for_empty_tree() {
    let t = MerkleTree::new(3).expect("new");
    let f = t.frontier().expect("frontier");
    for row in f.iter() {
        for slot in row.iter() {
            assert_eq!(*slot, [0u8; 32]);
        }
    }
}

#[test]
fn frontier_after_one_insert_holds_leaf_at_slot_0_level_0() {
    let mut t = MerkleTree::new(3).expect("new");
    let l = leaf(0x42);
    t.insert(l).unwrap();
    let f = t.frontier().unwrap();
    // After 1 insert, slot at lvl 0 = 1, parent_idx = 0.
    // frontier[0][0] = nodeAt(0, 0) = leaf.
    assert_eq!(f[0][0], l);
    // slot at lvl 1 = 0 → all zeros.
    for slot in f[1].iter() {
        assert_eq!(*slot, [0u8; 32]);
    }
}

#[test]
fn path_indices_match_quaternary_digits() {
    let t = MerkleTree::new(3).expect("new");
    // 17 = 1*16 + 0*4 + 1 → digits (LSB first) = [1, 0, 1].
    assert_eq!(t.path_indices_at(17), vec![1, 0, 1]);
    // 0 → [0,0,0]
    assert_eq!(t.path_indices_at(0), vec![0, 0, 0]);
    // 63 = 3*16 + 3*4 + 3 → [3,3,3]
    assert_eq!(t.path_indices_at(63), vec![3, 3, 3]);
}

#[test]
fn proof_recomputes_root_for_each_inserted_leaf() {
    let mut t = MerkleTree::new(2).expect("new"); // 16 capacity
    let mut leaves: Vec<Field> = Vec::new();
    for i in 0..16u64 {
        let l = leaf(0x100 + i);
        leaves.push(l);
        t.insert(l).unwrap();
    }
    let expected = t.root().unwrap();
    for (i, l) in leaves.iter().enumerate() {
        let p = t.proof(i).unwrap();
        // Recompute root from path.
        let mut cur = *l;
        for (lvl, sibs) in p.path_elements.iter().enumerate() {
            let slot = p.path_indices[lvl] as usize;
            let mut children = [[0u8; 32]; 4];
            let mut s = 0;
            #[allow(clippy::needless_range_loop)]
            for k in 0..4 {
                if k == slot {
                    children[k] = cur;
                } else {
                    children[k] = sibs[s];
                    s += 1;
                }
            }
            cur = hash_node(&children).unwrap();
            let _ = lvl;
        }
        assert_eq!(cur, expected, "leaf {} path mismatch", i);
    }
}

fn filled(depth: usize, n: usize) -> MerkleTree {
    let mut t = MerkleTree::new(depth).expect("new");
    for i in 0..n {
        t.insert(leaf(i as u64)).unwrap();
    }
    t
}

/// Bulk `extend`, a parallel bottom-up rebuild, must land on the same state as N
/// incremental `insert`s.
#[test]
fn extend_matches_incremental_insert() {
    for n in [0usize, 1, 3, 4, 5, 16, 17, 63, 64] {
        let incremental = filled(3, n);
        let mut bulk = MerkleTree::new(3).expect("new");
        bulk.extend((0..n).map(|i| leaf(i as u64))).unwrap();
        assert_eq!(
            bulk.root().unwrap(),
            incremental.root().unwrap(),
            "root mismatch at n={}",
            n
        );
        assert_eq!(
            bulk.frontier().unwrap(),
            incremental.frontier().unwrap(),
            "frontier mismatch at n={}",
            n
        );
    }
}

/// `extend` rebuilds only the levels above the leaves it appended, so a history
/// replayed in pages must land on the same state as one appended in a single
/// call. This is how both the relayer mirror and the indexer backfill read
/// `notes`.
#[test]
fn paged_extend_matches_a_single_extend() {
    for page in [1usize, 2, 3, 4, 7, 16] {
        let n = 37usize;
        let mut whole = MerkleTree::new(3).expect("new");
        whole.extend((0..n).map(|i| leaf(i as u64))).unwrap();

        let mut paged = MerkleTree::new(3).expect("new");
        for start in (0..n).step_by(page) {
            let end = (start + page).min(n);
            paged.extend((start..end).map(|i| leaf(i as u64))).unwrap();
        }
        assert_eq!(
            paged.root().unwrap(),
            whole.root().unwrap(),
            "root mismatch at page={}",
            page
        );
        assert_eq!(
            paged.frontier().unwrap(),
            whole.frontier().unwrap(),
            "frontier mismatch at page={}",
            page
        );
    }
}

/// `truncate_leaves` must leave the tree indistinguishable from one filled only
/// to the surviving leaf count. This is the relayer's rollback path, where a
/// stale internal node means a permanently divergent mirror.
#[test]
fn truncate_matches_tree_built_without_the_dropped_leaves() {
    for n in [1usize, 4, 5, 17, 33, 64] {
        for drop in [1usize, 2, 3] {
            if drop > n {
                continue;
            }
            let mut t = filled(3, n);
            t.truncate_leaves(drop).unwrap();
            let expected = filled(3, n - drop);
            assert_eq!(t.leaf_count(), n - drop);
            assert_eq!(
                t.root().unwrap(),
                expected.root().unwrap(),
                "root mismatch at n={} drop={}",
                n,
                drop
            );
            assert_eq!(
                t.frontier().unwrap(),
                expected.frontier().unwrap(),
                "frontier mismatch at n={} drop={}",
                n,
                drop
            );
        }
    }
}

/// Insert after rollback is the sequence a failed submit produces.
#[test]
fn reinsert_after_truncate_matches_direct_fill() {
    let mut t = filled(3, 10);
    t.truncate_leaves(2).unwrap();
    t.insert(leaf(0xaa)).unwrap();
    t.insert(leaf(0xbb)).unwrap();

    let mut expected = filled(3, 8);
    expected.insert(leaf(0xaa)).unwrap();
    expected.insert(leaf(0xbb)).unwrap();

    assert_eq!(t.root().unwrap(), expected.root().unwrap());
    assert_eq!(t.frontier().unwrap(), expected.frontier().unwrap());
}

#[test]
fn truncate_to_empty_restores_zero_root() {
    let empty = MerkleTree::new(3).expect("new");
    let mut t = filled(3, 7);
    t.truncate_leaves(7).unwrap();
    assert_eq!(t.leaf_count(), 0);
    assert_eq!(t.root().unwrap(), empty.root().unwrap());
    assert_eq!(t.frontier().unwrap(), empty.frontier().unwrap());
}

// --- Frontier ---------------------------------------------------------------
//
// `Frontier` exists to avoid materialising the tree, so its only real
// specification is that it is indistinguishable from `MerkleTree` at the same
// leaf count. These compare the two directly.

/// Root and frontier must agree after *every* insert, not just at the end. The
/// carry logic changes shape at each slot boundary (3→4, 15→16, 63→64), so an
/// end-state-only check would miss a group that was closed wrongly and then
/// papered over by later inserts.
#[test]
fn frontier_tracks_tree_at_every_leaf_count() {
    const DEPTH: usize = 3; // capacity 64: every level wraps at least once
    let mut tree = MerkleTree::new(DEPTH).expect("new");
    let mut front = Frontier::new(DEPTH).expect("new");

    assert_eq!(front.root(), tree.root().unwrap(), "empty root");
    assert_eq!(front.slots(), tree.frontier().unwrap(), "empty frontier");

    for i in 0..64u64 {
        let l = leaf(0x1000 + i);
        tree.insert(l).unwrap();
        front.push(l).unwrap();

        let n = i + 1;
        assert_eq!(front.leaf_count(), n, "leaf_count at n={n}");
        assert_eq!(front.root(), tree.root().unwrap(), "root at n={n}");
        assert_eq!(front.slots(), tree.frontier().unwrap(), "frontier at n={n}");
    }
}

/// Same check one level deeper and past a thousand leaves, where a level-2 or
/// level-3 group closes while lower levels are mid-group.
#[test]
fn frontier_tracks_tree_over_a_deeper_run() {
    const DEPTH: usize = 5; // capacity 1024
    let mut tree = MerkleTree::new(DEPTH).expect("new");
    let mut front = Frontier::new(DEPTH).expect("new");
    for i in 0..1000u64 {
        let l = leaf(i);
        tree.insert(l).unwrap();
        front.push(l).unwrap();
    }
    assert_eq!(front.root(), tree.root().unwrap());
    assert_eq!(front.slots(), tree.frontier().unwrap());
}

/// `resume` is the restart path: the indexer persists `(leaf_count, slots)` and
/// nothing else, so a resumed frontier must keep producing the same roots as one
/// that never stopped. Checked at slot boundaries and either side of them.
#[test]
fn frontier_resumes_from_persisted_state() {
    const DEPTH: usize = 4; // capacity 256
    for cut in [0usize, 1, 3, 4, 5, 15, 16, 17, 63, 64, 65, 100] {
        let mut continuous = Frontier::new(DEPTH).expect("new");
        for i in 0..cut {
            continuous.push(leaf(i as u64)).unwrap();
        }

        let mut resumed = Frontier::resume(
            DEPTH,
            continuous.leaf_count(),
            continuous.slots(),
            continuous.root(),
        )
        .expect("resume");
        assert_eq!(
            resumed.root(),
            continuous.root(),
            "resumed root at cut={cut}"
        );

        // And it must keep advancing identically, not merely start correct.
        for i in cut..cut + 9 {
            let l = leaf(i as u64);
            resumed.push(l).unwrap();
            continuous.push(l).unwrap();
            assert_eq!(
                resumed.root(),
                continuous.root(),
                "root after resume at cut={cut}, i={i}"
            );
            assert_eq!(
                resumed.slots(),
                continuous.slots(),
                "slots after resume at cut={cut}, i={i}"
            );
        }
    }
}

#[test]
fn frontier_resume_rejects_wrong_slot_count() {
    let empty = Frontier::new(4).expect("new");
    assert!(Frontier::resume(4, 0, vec![[[0u8; 32]; 3]; 3], empty.root()).is_err());
}

/// The `root` and `frontier` columns are written together but read as two
/// values, so a row that mixes one state's root with another's slots has to be
/// refused rather than folded onto: every root after it would be wrong, and
/// nothing downstream can tell.
#[test]
fn frontier_resume_rejects_a_root_that_disagrees_with_the_slots() {
    const DEPTH: usize = 4;
    let mut f = Frontier::new(DEPTH).expect("new");
    f.extend((0..37u64).map(leaf)).unwrap();

    // The root the row held one leaf earlier: well-formed, and wrong.
    let mut stale = Frontier::new(DEPTH).expect("new");
    stale.extend((0..36u64).map(leaf)).unwrap();

    match Frontier::resume(DEPTH, f.leaf_count(), f.slots(), stale.root()) {
        Err(TreeError::RootMismatch { .. }) => {}
        Err(other) => panic!("expected RootMismatch, got {other:?}"),
        Ok(_) => panic!("a mismatched root must not resume"),
    }
}

/// An exactly full tree stores the same all-zero frontier as an empty one -- at
/// capacity every level's slot is 0, so there are no left siblings to record --
/// which is why `resume` takes the stored root as given at that one leaf count
/// instead of folding it. Without the exemption a full tree resumes as an empty
/// one and every root after it is wrong.
#[test]
fn frontier_resumes_an_exactly_full_tree() {
    const DEPTH: usize = 3; // capacity 64
    let mut tree = MerkleTree::new(DEPTH).expect("new");
    let mut full = Frontier::new(DEPTH).expect("new");
    let leaves: Vec<Field> = (0..64u64).map(leaf).collect();
    tree.extend(leaves.clone()).unwrap();
    full.extend(leaves).unwrap();

    let empty = Frontier::new(DEPTH).expect("new");
    assert_eq!(
        full.slots(),
        empty.slots(),
        "a full frontier records no slots"
    );
    assert_ne!(full.root(), empty.root(), "but it is not the empty tree");

    let resumed =
        Frontier::resume(DEPTH, 64, full.slots(), full.root()).expect("resume at capacity");
    assert_eq!(resumed.root(), tree.root().unwrap());
    assert_eq!(resumed.leaf_count(), 64);
}

/// Past `ARITY^depth` the leaf index wraps onto leaf 0 and the root silently
/// stops matching the chain, so the overflow must be an error rather than a
/// modular index.
#[test]
fn frontier_rejects_leaves_past_capacity() {
    const DEPTH: usize = 2; // capacity 16
    let mut f = Frontier::new(DEPTH).expect("new");
    for i in 0..16u64 {
        f.push(leaf(i)).expect("within capacity");
    }
    assert!(f.push(leaf(99)).is_err());
    assert_eq!(f.leaf_count(), 16, "a rejected push must not advance");
}

#[test]
fn leaf_hash_matches_a_direct_poseidon_fold() {
    use ark_ed_on_bn254::Fq;
    let cm = leaf(0xc0ffee);
    let x = leaf(7);
    let y = leaf(9);
    let expected = super::hash::fq_to_be(
        crate::poseidon::hash(&[
            Fq::from(TAG_LEAF),
            super::hash::be_to_fq(&cm),
            super::hash::be_to_fq(&x),
            super::hash::be_to_fq(&y),
        ])
        .unwrap(),
    );
    assert_eq!(leaf_hash(&cm, &x, &y).unwrap(), expected);
}

#[test]
fn frontier_codec_round_trips() {
    const DEPTH: usize = 4;
    let mut f = Frontier::new(DEPTH).expect("new");
    for i in 0..37u64 {
        f.push(leaf(i)).unwrap();
    }
    let bytes = encode_frontier(&f.slots());
    assert_eq!(bytes.len(), DEPTH * 3 * 32);
    assert_eq!(decode_frontier(DEPTH, &bytes).unwrap(), f.slots());
}

#[test]
fn frontier_decode_rejects_a_wrong_length() {
    assert!(decode_frontier(4, &[0u8; 95]).is_err());
    assert!(decode_frontier(4, &[0u8; 384 + 1]).is_err());
}

/// `extend` is the batch path the indexer takes per tick, and the whole point of
/// it is that it does *not* fold the root once per leaf. It therefore has to be
/// proved equal to a `push` loop rather than assumed: every batch size against
/// every starting offset, so a group closing mid-batch, a batch landing exactly
/// on a boundary, and a batch spanning several levels are all covered.
#[test]
fn frontier_extend_matches_a_push_loop() {
    const DEPTH: usize = 4; // capacity 256
    for start in [0usize, 1, 3, 4, 5, 15, 16, 17, 63, 64, 65] {
        for batch in [1usize, 2, 3, 4, 5, 7, 16, 17, 64, 65] {
            let mut looped = Frontier::new(DEPTH).expect("new");
            for i in 0..start {
                looped.push(leaf(i as u64)).unwrap();
            }
            let mut batched = looped.clone();

            let leaves: Vec<Field> = (start..start + batch).map(|i| leaf(i as u64)).collect();
            for l in &leaves {
                looped.push(*l).unwrap();
            }
            batched.extend(leaves).unwrap();

            assert_eq!(
                batched.leaf_count(),
                looped.leaf_count(),
                "leaf_count at start={start} batch={batch}"
            );
            assert_eq!(
                batched.root(),
                looped.root(),
                "root at start={start} batch={batch}"
            );
            assert_eq!(
                batched.slots(),
                looped.slots(),
                "slots at start={start} batch={batch}"
            );
        }
    }
}

/// The batch path must agree with the materialised tree too, not just with the
/// serial frontier: `slots()` is what gets persisted and compared against the
/// root the chain published.
#[test]
fn frontier_extend_matches_the_tree() {
    const DEPTH: usize = 5; // capacity 1024
    let mut tree = MerkleTree::new(DEPTH).expect("new");
    let mut front = Frontier::new(DEPTH).expect("new");
    let mut next = 0u64;
    // Uneven pages, so batch boundaries land off the group boundaries.
    for page in [1u64, 7, 3, 64, 5, 100, 256, 33] {
        let leaves: Vec<Field> = (next..next + page).map(leaf).collect();
        tree.extend(leaves.clone()).unwrap();
        front.extend(leaves).unwrap();
        next += page;
        assert_eq!(front.root(), tree.root().unwrap(), "root at n={next}");
        assert_eq!(
            front.slots(),
            tree.frontier().unwrap(),
            "frontier at n={next}"
        );
    }
}

/// An exactly-full tree is the one leaf count whose root is not a fold over
/// `slots`: every level's slot is 0, so the fold finds no left siblings and
/// returns the empty root. `extend` keeps the top group's hash instead.
#[test]
fn frontier_extend_roots_an_exactly_full_tree() {
    const DEPTH: usize = 3; // capacity 64
    let mut tree = MerkleTree::new(DEPTH).expect("new");
    let mut front = Frontier::new(DEPTH).expect("new");
    let leaves: Vec<Field> = (0..64u64).map(leaf).collect();
    tree.extend(leaves.clone()).unwrap();
    front.extend(leaves).unwrap();
    assert_eq!(front.leaf_count(), 64);
    assert_eq!(front.root(), tree.root().unwrap());
}

/// A batch past capacity must be refused whole. Rejecting it leaf by leaf would
/// leave the frontier holding a prefix of the batch, which no caller expects
/// after an error.
#[test]
fn frontier_extend_rejects_a_batch_past_capacity() {
    const DEPTH: usize = 2; // capacity 16
    let mut f = Frontier::new(DEPTH).expect("new");
    f.extend((0..10u64).map(leaf)).expect("within capacity");
    let before = f.slots();
    assert!(f.extend((10..20u64).map(leaf)).is_err());
    assert_eq!(f.leaf_count(), 10, "a rejected batch must not advance");
    assert_eq!(f.slots(), before, "a rejected batch must not mutate slots");
}

/// The rayon fan-out in `MerkleTree::extend` and `Frontier::extend` only opens
/// past a threshold, so both sides of it need a case; a batch big enough to
/// split has to land on the same root as one that stays serial.
#[test]
fn frontier_extend_agrees_across_the_parallel_threshold() {
    const DEPTH: usize = 6; // capacity 4096
    let leaves: Vec<Field> = (0..2000u64).map(leaf).collect();

    let mut serial = Frontier::new(DEPTH).expect("new");
    for l in &leaves {
        serial.extend([*l]).unwrap();
    }
    let mut parallel = Frontier::new(DEPTH).expect("new");
    parallel.extend(leaves).unwrap();

    assert_eq!(parallel.root(), serial.root());
    assert_eq!(parallel.slots(), serial.slots());
}
