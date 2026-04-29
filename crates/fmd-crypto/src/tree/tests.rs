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
    let lvl1 = hash_node(&z, &z, &z, &z).unwrap();
    let expected = hash_node(&lvl1, &lvl1, &lvl1, &lvl1).unwrap();
    assert_eq!(r, expected);
}

#[test]
fn single_leaf_root_matches_direct() {
    let mut t = MerkleTree::new(2).expect("new");
    let l = leaf(0xabcd);
    t.insert(l);
    let z = [0u8; 32];
    let bottom0 = hash_node(&l, &z, &z, &z).unwrap();
    let bottom_z = hash_node(&z, &z, &z, &z).unwrap();
    let expected = hash_node(&bottom0, &bottom_z, &bottom_z, &bottom_z).unwrap();
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
    t.insert(l);
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
        t.insert(l);
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
            cur = hash_node(&children[0], &children[1], &children[2], &children[3]).unwrap();
            let _ = lvl;
        }
        assert_eq!(cur, expected, "leaf {} path mismatch", i);
    }
}

#[test]
fn root_par_matches_root_across_fill_levels() {
    for n in [0usize, 1, 3, 4, 5, 16, 17, 63, 64, 100] {
        let mut t = MerkleTree::new(3).expect("new"); // 64 cap
        if n > 64 {
            continue;
        }
        for i in 0..n {
            t.insert(leaf(i as u64));
        }
        assert_eq!(
            t.root().unwrap(),
            t.root_par().unwrap(),
            "mismatch at n={}",
            n
        );
    }
}
