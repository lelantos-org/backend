//! Diagnostic: prove a published golden vector with a real zkey and dump the
//! result in snarkjs shape, so it can be verified with the snarkjs CLI.
//!
//! Skipped unless `ZKEY_COMPAT_DIR` points at a directory holding
//! `tree_update_batch.wcd`, `tree_update_batch_final.zkey` and a `vector.json`
//! carrying the published vector. Writes `proof.json` and `public.json` next to
//! them.
//!
//! An integration test rather than a unit one: it exercises only the public API,
//! and it is the closest thing here to how a caller drives the prover.

use groth16::{Groth16Prover, Priority, TreeUpdateBatchProver, TreeUpdateBatchWitness};
use std::path::Path;

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_str().expect("string").to_string())
        .collect()
}

#[tokio::test]
async fn prove_published_vector_and_dump() {
    let Ok(dir) = std::env::var("ZKEY_COMPAT_DIR") else {
        eprintln!("ZKEY_COMPAT_DIR unset; skipping");
        return;
    };
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = Path::new(&dir);
    let vector: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("vector.json")).unwrap()).unwrap();
    let w = &vector["vectors"][0]["witness"];

    let witness = TreeUpdateBatchWitness {
        z: w["z"].as_str().unwrap().into(),
        old_root: w["old_root"].as_str().unwrap().into(),
        new_root: w["new_root"].as_str().unwrap().into(),
        start_index: w["start_index"].as_str().unwrap().into(),
        actual_count: w["actual_count"].as_str().unwrap().into(),
        cms: strs(&w["cms"]),
        cv_dep: w["cv_dep"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let a = strs(p);
                [a[0].clone(), a[1].clone()]
            })
            .collect(),
        leaf_asset: strs(&w["leaf_asset"]),
        leaf_public_in: strs(&w["leaf_public_in"]),
        is_deposit: strs(&w["is_deposit"]),
        rcv: strs(&w["rcv"]),
        frontier_in: w["frontier_in"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let a = strs(r);
                [a[0].clone(), a[1].clone(), a[2].clone()]
            })
            .collect(),
    };

    let prover = Groth16Prover::new(
        &dir.join("tree_update_batch.wcd"),
        &dir.join("tree_update_batch_final.zkey"),
    )
    .expect("load zkey");
    let p = prover.prove(witness, Priority::Spend).await.expect("prove");

    std::fs::write(
        dir.join("proof.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "pi_a": p.pi_a, "pi_b": p.pi_b, "pi_c": p.pi_c,
            "protocol": "groth16", "curve": "bn128",
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("public.json"),
        serde_json::to_string_pretty(&p.public_signals).unwrap(),
    )
    .unwrap();
    eprintln!("wrote proof.json / public.json to {}", dir.display());
}
