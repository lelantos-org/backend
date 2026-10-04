//! The relayer's own batch path, proved against the shipped circuit.
//!
//! `tree_update_batch.circom` pins every `frontier_in` slot no root reads to
//! zero, so a relayer that hands the prover a frontier with a stale value in an
//! unread slot cannot prove at all. The mirror is built to zero those slots
//! (`Frontier::slots` masks them), and this is the test that the whole path
//! agrees: `TreeMirror::reserve_and_advance_batch` reserves a slot for
//! `PaddedBatch::leaves`, `witness::challenge` derives the digest and the
//! challenge, `witness::build` turns it into circuit inputs, and `Groth16Prover`
//! computes the witness through the native `.wcd` graph, proves and verifies —
//! the exact sequence a spend and a flush run, with no hand-built frontier
//! anywhere. The proof's public signals are checked against the relayer's own
//! digest and challenge, which are what it puts in calldata.
//!
//! The starts are chosen for their frontiers: empty, one filled slot at several
//! levels, every slot filled at the lowest levels, and a batch straddling a
//! level-5 boundary. The counts cover a spend (`TRANSACT_OUT`), an odd count, a
//! full batch and a flush of two deposits.
//!
//! Skipped unless `ZKEY_COMPAT_DIR` holds `tree_update_batch.wcd` and
//! `tree_update_batch_final.zkey` from one release — `stack/circuits` after
//! `just fetch-circuits`:
//!
//! ```text
//! ZKEY_COMPAT_DIR=../../stack/circuits cargo test -p relayer --release \
//!     --test batch_prove_liveness
//! ```

use std::path::Path;

use alloy::primitives::FixedBytes;
use groth16::{Groth16Prover, Priority, TreeUpdateBatchProof, TreeUpdateBatchProver};
use relayer::domain::batch::PaddedBatch;
use relayer::domain::deposit::EscrowLeaf;
use relayer::domain::fiat_shamir::BatchChallenge;
use relayer::services::tree::{AdvancedState, ReservedSlot, TreeMirror};
use relayer::services::witness;

const CHAIN_ID: i64 = 31337;
const MAX_L: usize = 8;

/// A canonical field element, distinct per `n`.
fn cm(n: u64) -> [u8; 32] {
    let mut f = [0u8; 32];
    f[24..].copy_from_slice(&(n + 1).to_be_bytes());
    f
}

/// Advance `m` to `leaves` committed leaves in full batches.
fn fill_to(m: &mut TreeMirror, leaves: u64, next: &mut u64) {
    while m.committed_count() < leaves {
        let take = (leaves - m.committed_count()).min(MAX_L as u64);
        let batch: Vec<_> = (0..take)
            .map(|_| {
                *next += 1;
                cm(*next)
            })
            .collect();
        m.reserve_and_advance_batch(&batch).expect("prefill");
    }
}

/// Reserve the leaves `batch` inserts, as the batcher does.
fn reserve(m: &mut TreeMirror, batch: &PaddedBatch) -> (ReservedSlot, AdvancedState) {
    let leaves = batch.leaves().expect("canonical leaves");
    m.reserve_and_advance_batch(&leaves).expect("reserve")
}

/// Prove `batch` at `slot` the way the batcher does, and check the circuit's
/// public signals `[y, digest, z]` against what the relayer derived.
async fn prove(
    prover: &Groth16Prover,
    slot: &ReservedSlot,
    advanced: &AdvancedState,
    batch: &PaddedBatch,
) -> Result<(TreeUpdateBatchProof, BatchChallenge), String> {
    let challenge = witness::challenge(slot, advanced, batch).map_err(|e| e.to_string())?;
    let witness = witness::build(slot, advanced, batch, challenge.z);
    // `prove` verifies the proof it produced, so an unsatisfiable witness — a
    // stale unread frontier slot among them — fails here.
    let proof = prover
        .prove(witness, Priority::Spend)
        .await
        .map_err(|e| e.to_string())?;
    Ok((proof, challenge))
}

fn assert_signals(proof: &TreeUpdateBatchProof, challenge: &BatchChallenge, case: &str) {
    assert_eq!(
        BatchChallenge::from_public_signals(&proof.public_signals).as_ref(),
        Some(challenge),
        "{case}: the circuit's digest is the one the relayer puts in calldata, \
         and z is passed through"
    );
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
        let cms: Vec<FixedBytes<32>> = (0..count)
            .map(|_| {
                next += 1;
                FixedBytes::from(cm(next))
            })
            .collect();
        let batch = PaddedBatch::from_spend(&cms);
        let (slot, advanced) = reserve(&mut m, &batch);
        assert_eq!(slot.start_index, start);

        let case = format!("start {start}, count {count}");
        let (proof, challenge) = prove(&prover, &slot, &advanced, &batch)
            .await
            .unwrap_or_else(|e| panic!("{case}: {e}"));
        assert_signals(&proof, &challenge, &case);
    }

    // A flush of two deposits: every slot carries `inner`, and the mirror holds
    // the leaf the circuit builds from it and the public amount. The second fee
    // note is worthless, so its leaf is under asset 0.
    let escrow = |asset_id, public_in, n: &mut u64| {
        *n += 1;
        EscrowLeaf {
            inner: cm(*n),
            asset_id,
            public_in,
        }
    };
    let deposits = [
        escrow(7, 1_000, &mut next),
        escrow(9, 250, &mut next),
        escrow(7, 42, &mut next),
        escrow(0, 0, &mut next),
    ];
    let batch = PaddedBatch::from_deposits(&deposits);
    let (slot, advanced) = reserve(&mut m, &batch);
    let (proof, challenge) = prove(&prover, &slot, &advanced, &batch)
        .await
        .unwrap_or_else(|e| panic!("deposit batch: {e}"));
    assert_signals(&proof, &challenge, "deposit batch");

    // The control that makes the passes above mean something: the same path with
    // one stale value in a slot no root reads must not prove. At start 21 level 0
    // has digit 1, so slot 1 is unread.
    let mut m = TreeMirror::new(CHAIN_ID).expect("mirror");
    let mut next = 0u64;
    fill_to(&mut m, 21, &mut next);
    let batch = PaddedBatch::from_spend(&[FixedBytes::from(cm(next + 1))]);
    let (mut slot, advanced) = reserve(&mut m, &batch);
    assert_eq!(
        slot.old_frontier[0][1], [0u8; 32],
        "the mirror zeroes unread slots"
    );
    slot.old_frontier[0][1] = cm(999);
    assert!(
        prove(&prover, &slot, &advanced, &batch).await.is_err(),
        "a stale unread frontier slot must not prove"
    );
}
