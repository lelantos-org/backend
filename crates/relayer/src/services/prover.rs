// `tree_update_batch` prover. In-process Groth16 over ark-bn254 against the
// snarkjs-compatible `.zkey`; the proving key is parsed once at startup.

use crate::domain::error::{AppError, AppResult};
use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tracing::info;

use ark_bn254::{Bn254, Fr};
use ark_circom::read_zkey;
use ark_circom::{CircomCircuit, CircomConfig, CircomReduction};
use ark_ff::PrimeField;
use ark_groth16::{Groth16, ProvingKey};
use ark_snark::SNARK;
use num_bigint::BigInt;
use parking_lot::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct TreeUpdateBatchWitness {
    /// Decimal field-element strings (snarkjs convention). Caller composes
    /// the witness shape that matches `circuits/src/tree_update_batch.circom`.
    pub z: String,
    pub old_root: String,
    pub new_root: String,
    pub start_index: String,
    pub actual_count: String,
    /// `MAX_L_BATCH` entries, leaf-indexed; padding (i ≥ actual_count)
    /// MUST be "0".
    pub cms: Vec<String>,
    /// `MAX_L_BATCH` Baby-Jubjub points (depositor-anchored value
    /// commitments). Padding entries MUST be "0".
    pub cv_dep: Vec<[String; 2]>,
    /// `MAX_L_BATCH` per-leaf publicAssetId. Padding "0".
    pub leaf_asset: Vec<String>,
    /// `MAX_L_BATCH` per-leaf publicIn. Padding "0".
    pub leaf_public_in: Vec<String>,
    /// `MAX_L_BATCH` 0/1 flags. 1 = deposit leaf (binding enforced).
    pub is_deposit: Vec<String>,
    /// `MAX_L_BATCH` private per-leaf rcv_dep. Padding "0".
    pub rcv: Vec<String>,
    pub frontier_in: Vec<[String; 3]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeUpdateBatchProof {
    pub pi_a: [String; 3],
    pub pi_b: [[String; 2]; 3],
    pub pi_c: [String; 3],
    pub public_signals: Vec<String>,
}

#[async_trait]
pub trait TreeUpdateBatchProver: Send + Sync {
    async fn prove(&self, witness: TreeUpdateBatchWitness) -> AppResult<TreeUpdateBatchProof>;
}

/// In-process Groth16 prover. The proving key (~150MB zkey) *and* the circom
/// config (wasm module + parsed r1cs) are loaded once at startup and reused;
/// a prove then costs only the witness calculation and the proof itself.
///
/// Proving is serialized — `Groth16::prove` already saturates the machine —
/// but the gate is a `Semaphore` held *outside* `spawn_blocking`, not a mutex
/// inside it. Queuing on a blocking-pool thread would let a burst of requests
/// park the whole pool, which the DB and everything else also draws from.
pub struct ArkCircomProver {
    pk: Arc<ProvingKey<Bn254>>,
    /// Guarded by `gate`, so the inner lock is always uncontended.
    cfg: Arc<Mutex<CircomConfig<Fr>>>,
    gate: Arc<Semaphore>,
}

impl ArkCircomProver {
    pub fn new(wasm_path: &PathBuf, r1cs_path: &PathBuf, zkey_path: &PathBuf) -> AppResult<Self> {
        let mut cfg = CircomConfig::<Fr>::new(wasm_path, r1cs_path)
            .map_err(|e| AppError::Prover(format!("circom config: {}", e)))?;
        cfg.sanity_check = true;
        let mut zk = std::fs::File::open(zkey_path)
            .map_err(|e| AppError::Prover(format!("open zkey: {}", e)))?;
        let (pk, _matrices) =
            read_zkey(&mut zk).map_err(|e| AppError::Prover(format!("read zkey: {}", e)))?;
        Ok(Self {
            pk: Arc::new(pk),
            cfg: Arc::new(Mutex::new(cfg)),
            gate: Arc::new(Semaphore::new(1)),
        })
    }
}

#[async_trait]
impl TreeUpdateBatchProver for ArkCircomProver {
    async fn prove(&self, witness: TreeUpdateBatchWitness) -> AppResult<TreeUpdateBatchProof> {
        let pk = self.pk.clone();
        let cfg = self.cfg.clone();

        info!(
            start_index = %witness.start_index,
            actual_count = %witness.actual_count,
            "ark-circom groth16 prove queued"
        );
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|e| AppError::Prover(format!("prove gate: {}", e)))?;

        let inputs = circom_inputs(&witness)?;

        let started = Instant::now();
        let result = tokio::task::spawn_blocking(move || -> AppResult<TreeUpdateBatchProof> {
            let mut cfg = cfg.lock();
            let circom = build_circuit(&mut cfg, inputs)?;

            let public_inputs_fr = circom
                .get_public_inputs()
                .ok_or_else(|| AppError::Prover("no public inputs".into()))?;

            let mut rng = rand::rngs::OsRng;
            let proof = Groth16::<Bn254, CircomReduction>::prove(&pk, circom, &mut rng)
                .map_err(|e| AppError::Prover(format!("groth16 prove: {}", e)))?;

            let pi_a = g1_to_dec(&proof.a);
            let pi_b = g2_to_dec(&proof.b);
            let pi_c = g1_to_dec(&proof.c);
            let public_signals = public_inputs_fr.iter().map(fr_to_dec).collect();

            Ok(TreeUpdateBatchProof {
                pi_a,
                pi_b,
                pi_c,
                public_signals,
            })
        })
        .await
        .map_err(|e| AppError::Prover(format!("prove join: {}", e)))??;

        info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "ark-circom prove ok"
        );
        Ok(result)
    }
}

/// `CircomBuilder::build` consumes its `CircomConfig`, which would force a
/// wasm + r1cs reload per prove. This is the same sequence against a borrowed
/// config, so the expensive parts stay resident.
fn build_circuit(
    cfg: &mut CircomConfig<Fr>,
    inputs: HashMap<String, Vec<BigInt>>,
) -> AppResult<CircomCircuit<Fr>> {
    let mut r1cs = cfg.r1cs.clone();
    // Disable the wire mapping, as `CircomBuilder::setup` does.
    r1cs.wire_mapping = None;
    let witness = cfg
        .wtns
        .calculate_witness_element::<Fr, _>(&mut cfg.store, inputs, cfg.sanity_check)
        .map_err(|e| AppError::Prover(format!("witness build: {}", e)))?;
    Ok(CircomCircuit {
        r1cs,
        witness: Some(witness),
    })
}

/// Witness → circom signal map, in the shape `tree_update_batch.circom`
/// declares. Kept separate from proving so the mapping is checkable without a
/// zkey; a wrong length here otherwise surfaces from circom as an opaque
/// witness-build failure.
fn circom_inputs(w: &TreeUpdateBatchWitness) -> AppResult<HashMap<String, Vec<BigInt>>> {
    let mut inputs: HashMap<String, Vec<BigInt>> = HashMap::new();
    let mut signal = |name: &str, decs: &mut dyn Iterator<Item = &str>| -> AppResult<()> {
        let values = decs
            .map(|d| {
                BigInt::from_str(d)
                    .map_err(|e| AppError::Prover(format!("signal '{name}' value '{d}': {e}")))
            })
            .collect::<AppResult<Vec<_>>>()?;
        if inputs.insert(name.to_string(), values).is_some() {
            return Err(AppError::Prover(format!("signal '{name}' set twice")));
        }
        Ok(())
    };

    signal("z", &mut std::iter::once(w.z.as_str()))?;
    signal("old_root", &mut std::iter::once(w.old_root.as_str()))?;
    signal("new_root", &mut std::iter::once(w.new_root.as_str()))?;
    signal("start_index", &mut std::iter::once(w.start_index.as_str()))?;
    signal(
        "actual_count",
        &mut std::iter::once(w.actual_count.as_str()),
    )?;
    signal("cms", &mut w.cms.iter().map(String::as_str))?;
    signal("cv_dep", &mut w.cv_dep.iter().flatten().map(String::as_str))?;
    signal("leaf_asset", &mut w.leaf_asset.iter().map(String::as_str))?;
    signal(
        "leaf_public_in",
        &mut w.leaf_public_in.iter().map(String::as_str),
    )?;
    signal("is_deposit", &mut w.is_deposit.iter().map(String::as_str))?;
    signal(
        "frontier_in",
        &mut w.frontier_in.iter().flatten().map(String::as_str),
    )?;
    signal("rcv", &mut w.rcv.iter().map(String::as_str))?;
    Ok(inputs)
}

/// Affine G1 → snarkjs proof shape: `[x, y, "1"]` in decimal.
fn g1_to_dec(p: &ark_bn254::G1Affine) -> [String; 3] {
    use ark_ec::AffineRepr;
    if p.is_zero() {
        return ["0".into(), "1".into(), "0".into()];
    }
    [fq_to_dec(&p.x), fq_to_dec(&p.y), "1".into()]
}

/// Affine G2 → snarkjs proof shape: `[[x_c0, x_c1], [y_c0, y_c1], ["1","0"]]`.
fn g2_to_dec(p: &ark_bn254::G2Affine) -> [[String; 2]; 3] {
    use ark_ec::AffineRepr;
    if p.is_zero() {
        return [
            ["0".into(), "0".into()],
            ["1".into(), "0".into()],
            ["0".into(), "0".into()],
        ];
    }
    [
        [fq_to_dec(&p.x.c0), fq_to_dec(&p.x.c1)],
        [fq_to_dec(&p.y.c0), fq_to_dec(&p.y.c1)],
        ["1".into(), "0".into()],
    ]
}

fn fq_to_dec(x: &ark_bn254::Fq) -> String {
    x.into_bigint().to_string()
}

fn fr_to_dec(x: &Fr) -> String {
    x.into_bigint().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::calldata::MAX_L_BATCH;
    use crate::domain::dto::TRANSACT_OUT;
    use crate::services::tree::{AdvancedState, ReservedSlot};
    use crate::services::witness;
    use alloy::primitives::{FixedBytes, U256};

    /// A spend witness: `TRANSACT_OUT` leaves, no deposit binding — the
    /// shape both single-spend pipelines produce.
    fn spend_witness() -> TreeUpdateBatchWitness {
        let slot = ReservedSlot {
            start_index: 4,
            old_root: [1u8; 32],
            old_frontier: vec![[[2u8; 32], [3u8; 32], [4u8; 32]]; 10],
        };
        let advanced = AdvancedState {
            new_root: [5u8; 32],
        };
        let cms: Vec<FixedBytes<32>> = (0..TRANSACT_OUT)
            .map(|i| FixedBytes::<32>::from([6u8 + i as u8; 32]))
            .collect();
        let cv_deps: Vec<[U256; 2]> = (0..TRANSACT_OUT)
            .map(|i| [U256::from(8u8 + i as u8), U256::from(9u8 + i as u8)])
            .collect();
        witness::build_spend(&slot, &advanced, &cms, &cv_deps, "12".to_string())
    }

    /// The circuit declares fixed-width arrays; a short or long signal is
    /// rejected deep inside circom with no useful message, so pin the widths.
    #[test]
    fn signal_widths_match_the_circuit_declaration() {
        let inputs = circom_inputs(&spend_witness()).unwrap();
        let width = |name: &str| {
            inputs
                .get(name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .len()
        };

        for scalar in ["z", "old_root", "new_root", "start_index", "actual_count"] {
            assert_eq!(width(scalar), 1, "{scalar}");
        }
        assert_eq!(width("cms"), MAX_L_BATCH);
        assert_eq!(width("cv_dep"), 2 * MAX_L_BATCH, "flattened BJJ points");
        assert_eq!(width("leaf_asset"), MAX_L_BATCH);
        assert_eq!(width("leaf_public_in"), MAX_L_BATCH);
        assert_eq!(width("is_deposit"), MAX_L_BATCH);
        assert_eq!(width("rcv"), MAX_L_BATCH);
        assert_eq!(width("frontier_in"), 3 * 10, "depth rows of 3 siblings");
    }

    #[test]
    fn every_declared_signal_is_supplied_and_no_others() {
        let inputs = circom_inputs(&spend_witness()).unwrap();
        let mut names: Vec<&str> = inputs.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "actual_count",
                "cms",
                "cv_dep",
                "frontier_in",
                "is_deposit",
                "leaf_asset",
                "leaf_public_in",
                "new_root",
                "old_root",
                "rcv",
                "start_index",
                "z",
            ]
        );
    }

    /// Padding slots must be zero — the circuit and the contract both enforce
    /// it, and a stray value there is otherwise invisible until the prove.
    #[test]
    fn padding_slots_are_zero() {
        let inputs = circom_inputs(&spend_witness()).unwrap();
        let zero = BigInt::from(0);
        for (name, from) in [("cms", TRANSACT_OUT), ("cv_dep", 2 * TRANSACT_OUT)] {
            for (i, v) in inputs[name].iter().enumerate().skip(from) {
                assert_eq!(*v, zero, "{name}[{i}] should be padding");
            }
        }
        for name in ["leaf_asset", "leaf_public_in", "is_deposit", "rcv"] {
            assert!(inputs[name].iter().all(|v| *v == zero), "{name}");
        }
    }

    #[test]
    fn a_non_numeric_signal_names_itself_in_the_error() {
        let mut w = spend_witness();
        w.cms[0] = "not-a-number".into();
        let err = circom_inputs(&w).unwrap_err();
        assert!(err.to_string().contains("cms"), "got {err}");
    }
}

#[cfg(test)]
mod zkey_compat {
    //! Diagnostic: prove a published golden vector with a real zkey and dump
    //! the result in snarkjs shape, so it can be verified with the snarkjs CLI.
    //!
    //! Skipped unless `ZKEY_COMPAT_DIR` points at a directory holding
    //! `tree_update_batch.{wasm,r1cs}`, `tree_update_batch_final.zkey` and
    //! `vector.json` (the published `tree-update-batch-8` vector). Writes
    //! `proof.json` / `public.json` next to them.

    use super::*;
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
        let dir = Path::new(&dir);
        let vector: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("vector.json")).unwrap())
                .unwrap();
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

        let prover = ArkCircomProver::new(
            &dir.join("tree_update_batch.wasm"),
            &dir.join("tree_update_batch.r1cs"),
            &dir.join("tree_update_batch_final.zkey"),
        )
        .expect("load zkey");
        let p = prover.prove(witness).await.expect("prove");

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
}
