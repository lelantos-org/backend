//! The relayer's own batch path, proved against the shipped circuit.
//!
//! `tree_update_batch.circom` pins every `frontier_in` slot no root reads to
//! zero, so a relayer that hands the prover a frontier with a stale value in an
//! unread slot cannot prove at all. The mirror is built to zero those slots
//! (`Frontier::slots` masks them), and this is the test that the whole path
//! agrees: `TreeMirror::reserve_and_advance_batch` reserves a slot,
//! `witness::build_spend` turns it into circuit inputs, and `Groth16Prover`
//! computes the witness through the native `.wcd` graph, proves and verifies —
//! the exact sequence a spend runs, with no hand-built frontier anywhere.
//!
//! The starts are chosen for their frontiers: empty, one filled slot at several
//! levels, every slot filled at the lowest levels, and a batch straddling a
//! level-5 boundary. The counts cover a spend (`TRANSACT_OUT`), an odd count and
//! a full batch.
//!
//! Skipped unless `ZKEY_COMPAT_DIR` holds `tree_update_batch.wcd` and
//! `tree_update_batch_final.zkey` from one build — `circuits/build` after
//! `just rebuild-batch` or `just setup-batch` plus `just build-graph`:
//!
//! ```text
//! ZKEY_COMPAT_DIR=../../../circuits/build cargo test -p relayer --release \
//!     --test batch_prove_liveness
//! ```

use std::path::Path;

use alloy::primitives::{FixedBytes, U256};
use groth16::{Groth16Prover, Priority, TreeUpdateBatchProver};
use relayer::services::tree::TreeMirror;
use relayer::services::witness::build_spend;

const CHAIN_ID: i64 = 31337;
const MAX_L: usize = 8;

/// A canonical field element, distinct per `n`.
fn cm(n: u64) -> [u8; 32] {
    let mut f = [0u8; 32];
    f[24..].copy_from_slice(&(n + 1).to_be_bytes());
    f
}

/// The Baby-Jubjub identity: on the curve, so it passes the circuit's
/// `BabyCheck` on every active spend slot.
fn identity() -> [U256; 2] {
    [U256::ZERO, U256::from(1u8)]
}

/// Advance `m` to `leaves` committed leaves in full batches.
fn fill_to(m: &mut TreeMirror, leaves: u64, next: &mut u64) {
    while m.committed_count() < leaves {
        let take = (leaves - m.committed_count()).min(MAX_L as u64);
        let batch: Vec<_> = (0..take)
            .map(|_| {
                *next += 1;
                (cm(*next), identity())
            })
            .collect();
        m.reserve_and_advance_batch(&batch).expect("prefill");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn relayer_witnesses_prove_at_every_frontier_shape() {
    let Ok(dir) = std::env::var("ZKEY_COMPAT_DIR") else {
        eprintln!("ZKEY_COMPAT_DIR unset; skipping");
        return;
    };
    let dir = Path::new(&dir);
    let prover = Groth16Prover::new(
        &dir.join("tree_update_batch.wcd"),
        &dir.join("tree_update_batch_final.zkey"),
    )
    .expect("load graph and zkey");

    // (start, count), in increasing start so one mirror serves every case.
    //   0     empty frontier
    //   6     digits 2,1: two filled slots at level 0, one at level 1
    //   21    digits 1,1,1: one filled slot at the three lowest levels
    //   63    digits 3,3,3: every slot filled at the three lowest levels
    //   255   digits 3,3,3,3, and the batch carries into level 4
    //   1021  4^5 - 3: a full batch straddling a level-5 boundary
    let cases: &[(u64, usize)] = &[(0, 6), (6, 3), (21, 6), (63, 1), (255, 8), (1021, 8)];

    let mut m = TreeMirror::new(CHAIN_ID).expect("mirror");
    let mut next = 0u64;
    for &(start, count) in cases {
        fill_to(&mut m, start, &mut next);
        let batch: Vec<_> = (0..count)
            .map(|_| {
                next += 1;
                (cm(next), identity())
            })
            .collect();
        let (slot, advanced) = m.reserve_and_advance_batch(&batch).expect("reserve");
        assert_eq!(slot.start_index, start);

        let cms: Vec<FixedBytes<32>> = batch.iter().map(|(c, _)| FixedBytes::from(*c)).collect();
        let cv_deps: Vec<[U256; 2]> = batch.iter().map(|(_, cv)| *cv).collect();
        let witness = build_spend(&slot, &advanced, &cms, &cv_deps, "1".to_string());

        // `prove` verifies the proof it produced, so an unsatisfiable witness —
        // a stale unread frontier slot among them — fails here.
        let proof = prover
            .prove(witness, Priority::Spend)
            .await
            .unwrap_or_else(|e| panic!("start {start}, count {count}: {e}"));
        assert_eq!(proof.public_signals.len(), 2, "[y, z]");
        assert_eq!(proof.public_signals[1], "1", "z is passed through");
    }

    // The control that makes the passes above mean something: the same path with
    // one stale value in a slot no root reads must not prove. At start 21 level 0
    // has digit 1, so slot 1 is unread.
    let mut m = TreeMirror::new(CHAIN_ID).expect("mirror");
    let mut next = 0u64;
    fill_to(&mut m, 21, &mut next);
    let batch = [(cm(next + 1), identity())];
    let (mut slot, advanced) = m.reserve_and_advance_batch(&batch).expect("reserve");
    assert_eq!(
        slot.old_frontier[0][1], [0u8; 32],
        "the mirror zeroes unread slots"
    );
    slot.old_frontier[0][1] = cm(999);
    let witness = build_spend(
        &slot,
        &advanced,
        &[FixedBytes::from(batch[0].0)],
        &[batch[0].1],
        "1".to_string(),
    );
    assert!(
        prover.prove(witness, Priority::Spend).await.is_err(),
        "a stale unread frontier slot must not prove"
    );
}
