//! `tree_update_batch` prover: in-process Groth16 over ark-bn254 against the
//! snarkjs-compatible `.zkey`, with the proving key parsed once at startup.

use crate::domain::error::{AppError, AppResult, ErrorContext};
use crate::services::witness_calc::{self, WitnessCalculator};
use async_trait::async_trait;
use serde::Serialize;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::info;

use crate::services::qap::CircomReduction;
use crate::services::zkey::read_zkey;
use ark_bn254::{Bn254, Fr};
use ark_ff::{PrimeField, UniformRand};
use ark_groth16::{Groth16, PreparedVerifyingKey, Proof, ProvingKey};
use ark_relations::utils::matrix::Matrix;
use ark_snark::SNARK;
use num_bigint::BigInt;

#[derive(Debug, Clone, Serialize)]
pub struct TreeUpdateBatchWitness {
    /// Decimal field-element strings, the snarkjs convention. The caller composes
    /// the witness shape `circuits/src/tree_update_batch.circom` declares.
    pub z: String,
    pub old_root: String,
    pub new_root: String,
    pub start_index: String,
    pub actual_count: String,
    /// `MAX_L_BATCH` leaf-indexed entries. Padding, where `i >= actual_count`,
    /// must be "0".
    pub cms: Vec<String>,
    /// `MAX_L_BATCH` Baby-Jubjub points, the depositor-anchored value
    /// commitments. Padding entries must be "0".
    pub cv_dep: Vec<[String; 2]>,
    /// `MAX_L_BATCH` per-leaf `publicAssetId`. Padding is "0".
    pub leaf_asset: Vec<String>,
    /// `MAX_L_BATCH` per-leaf `publicIn`. Padding is "0".
    pub leaf_public_in: Vec<String>,
    /// `MAX_L_BATCH` 0/1 flags, where 1 marks a deposit leaf whose binding the
    /// circuit enforces.
    pub is_deposit: Vec<String>,
    /// `MAX_L_BATCH` private per-leaf `rcv_dep`. Padding is "0".
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

/// Who is waiting on a proof. A spend has an HTTP caller blocked on it, while a
/// flush is a background tick that returns in seconds, so it yields the prover
/// rather than queueing ahead of a spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Spend,
    Flush,
}

#[async_trait]
pub trait TreeUpdateBatchProver: Send + Sync {
    async fn prove(
        &self,
        witness: TreeUpdateBatchWitness,
        priority: Priority,
    ) -> AppResult<TreeUpdateBatchProof>;

    /// Whether a [`Priority::Flush`] prove would be refused right now.
    ///
    /// Advisory: the answer can go stale before the caller acts on it, at no more
    /// cost than the caller would have paid anyway. It lets a background tick bail
    /// out before taking a chain's tree-mirror lock rather than reserving leaves
    /// and unwinding them.
    fn is_busy(&self) -> bool {
        false
    }
}

/// Everything a proof needs from the zkey, parsed once at startup.
///
/// The zkey already carries the A/B/C matrices, so
/// `create_proof_with_reduction_and_matrices` receives them directly.
/// `Groth16::prove` over a `CircomCircuit` would instead rebuild them from the
/// `.r1cs` on every proof: cloning the constraint list, re-emitting every linear
/// combination, inlining them, then materialising the matrices again inside the
/// QAP witness map.
struct Groth16Params {
    pk: ProvingKey<Bn254>,
    /// `[a, b, c]`, in the order `create_proof_with_reduction_and_matrices`
    /// indexes them.
    matrices: Vec<Matrix<Fr>>,
    /// Verifies the proof just produced, in place of circom's per-signal sanity
    /// check: three pairings against a whole-witness re-check.
    pvk: PreparedVerifyingKey<Bn254>,
    /// Length of the public witness prefix: the leading `1` plus the circuit's
    /// public outputs and inputs.
    num_inputs: usize,
    num_constraints: usize,
}

impl Groth16Params {
    fn load(zkey_path: &Path) -> AppResult<Self> {
        let mut file = std::fs::File::open(zkey_path).prover("open zkey")?;
        let (pk, m) = read_zkey(&mut file).prover("read zkey")?;
        let pvk = Groth16::<Bn254, CircomReduction>::process_vk(&pk.vk).prover("process vk")?;
        Ok(Self {
            pk,
            matrices: vec![m.a, m.b, m.c],
            pvk,
            num_inputs: m.num_instance_variables,
            num_constraints: m.num_constraints,
        })
    }

    /// The public prefix of a full witness.
    ///
    /// Element 0 is circom's constant `1`, which the verifier supplies itself.
    /// This is the slice `CircomCircuit::get_public_inputs` returns once the wire
    /// mapping is dropped, so proving from matrices leaves the signals reaching
    /// the contract unchanged.
    fn public_inputs<'w>(&self, witness: &'w [Fr]) -> AppResult<&'w [Fr]> {
        witness
            .get(1..self.num_inputs)
            .ok_or_else(|| AppError::Prover("witness shorter than its public inputs".into()))
    }

    fn prove(&self, witness: &[Fr]) -> AppResult<Proof<Bn254>> {
        let mut rng = rand::rngs::OsRng;
        let (r, s) = (Fr::rand(&mut rng), Fr::rand(&mut rng));
        Groth16::<Bn254, CircomReduction>::create_proof_with_reduction_and_matrices(
            &self.pk,
            r,
            s,
            &self.matrices,
            self.num_inputs,
            self.num_constraints,
            witness,
        )
        .prover("groth16 prove")
    }

    /// Check our own output before it becomes calldata.
    ///
    /// Stands in for circom's `sanity_check`: a witness that does not satisfy the
    /// constraints cannot yield a verifying proof, and failing here is a failed
    /// submission rather than an opaque on-chain revert.
    fn verify(&self, public_inputs: &[Fr], proof: &Proof<Bn254>) -> AppResult<()> {
        let ok = Groth16::<Bn254, CircomReduction>::verify_with_processed_vk(
            &self.pvk,
            public_inputs,
            proof,
        )
        .prover("verify own proof")?;
        if !ok {
            return Err(AppError::Prover("own proof failed verification".into()));
        }
        Ok(())
    }
}

/// Where one proof spent its time. `elapsed_ms` alone cannot distinguish the
/// witness build from the MSMs, which have different fixes.
#[derive(Debug, Default)]
struct Timings {
    witness_ms: u64,
    groth16_ms: u64,
    verify_ms: u64,
}

/// Run `f`, recording how long it took into `slot`.
fn timed<T>(slot: &mut u64, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    *slot = start.elapsed().as_millis() as u64;
    out
}

/// In-process Groth16 prover. The proving key, a ~150 MB zkey, the A/B/C matrices
/// it carries, and the witness-calculation graph are loaded once at startup and
/// reused, so a prove costs only the witness calculation and the proof itself.
///
/// Proving is serialised, since the MSMs already saturate the machine, but the
/// gate is a `Semaphore` held outside `spawn_blocking` rather than a mutex inside
/// it: queueing on a blocking-pool thread would let a burst of requests park the
/// whole pool, which the database and everything else also draw from.
pub struct Groth16Prover {
    params: Arc<Groth16Params>,
    wtns: Arc<WitnessCalculator>,
    gate: Semaphore,
}

impl Groth16Prover {
    /// `graph_path` is the `.wcd` witness-calculation graph `just build-graph`
    /// emits. The constraint matrices come from the zkey, which already carries
    /// them, so no `.r1cs` is needed here.
    pub fn new(graph_path: &Path, zkey_path: &Path) -> AppResult<Self> {
        let wtns = WitnessCalculator::new(graph_path)?;
        let params = Groth16Params::load(zkey_path)?;
        info!(
            threads = rayon::current_num_threads(),
            available_parallelism = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0),
            num_constraints = params.num_constraints,
            num_inputs = params.num_inputs,
            "groth16 prover ready"
        );
        Ok(Self {
            params: Arc::new(params),
            wtns: Arc::new(wtns),
            gate: Semaphore::new(1),
        })
    }

    /// Take the prover.
    ///
    /// The gate is FIFO, so a background flush that queued first would make a
    /// spend wait a whole proof. [`Priority::Flush`] therefore never queues: it
    /// takes the permit only if free, and its worker retries on the next tick.
    /// Within one chain the mirror mutex already excludes the two; this covers the
    /// cross-chain case, where several chains share one prover.
    async fn acquire(&self, priority: Priority) -> AppResult<SemaphorePermit<'_>> {
        match priority {
            Priority::Spend => self.gate.acquire().await.prover("prove gate"),
            Priority::Flush => self.gate.try_acquire().map_err(|_| AppError::ProverBusy),
        }
    }
}

/// Witness to proof, start to finish. Blocking and CPU-bound; the caller runs it
/// on a blocking thread while holding the prover's permit.
fn run_proof(
    params: &Groth16Params,
    wtns: &WitnessCalculator,
    inputs: witness_calc::Inputs,
) -> AppResult<(TreeUpdateBatchProof, Timings)> {
    let mut timings = Timings::default();

    let witness = timed(&mut timings.witness_ms, || wtns.calculate(inputs))?;
    let public_inputs = params.public_inputs(&witness)?;
    let proof = timed(&mut timings.groth16_ms, || params.prove(&witness))?;
    timed(&mut timings.verify_ms, || {
        params.verify(public_inputs, &proof)
    })?;

    Ok((
        TreeUpdateBatchProof {
            pi_a: g1_to_dec(&proof.a),
            pi_b: g2_to_dec(&proof.b),
            pi_c: g1_to_dec(&proof.c),
            public_signals: public_inputs.iter().map(fr_to_dec).collect(),
        },
        timings,
    ))
}

#[async_trait]
impl TreeUpdateBatchProver for Groth16Prover {
    fn is_busy(&self) -> bool {
        self.gate.available_permits() == 0
    }

    async fn prove(
        &self,
        witness: TreeUpdateBatchWitness,
        priority: Priority,
    ) -> AppResult<TreeUpdateBatchProof> {
        info!(
            start_index = %witness.start_index,
            actual_count = %witness.actual_count,
            ?priority,
            "groth16 prove queued"
        );

        let queued = Instant::now();
        let _permit = self.acquire(priority).await?;
        let queue_wait_ms = queued.elapsed().as_millis() as u64;

        let inputs = circom_inputs(&witness)?;
        let (params, wtns) = (self.params.clone(), self.wtns.clone());

        let started = Instant::now();
        let (proof, timings) =
            tokio::task::spawn_blocking(move || run_proof(&params, &wtns, inputs))
                .await
                .prover("prove join")??;

        let Timings {
            witness_ms,
            groth16_ms,
            verify_ms,
        } = timings;
        info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            queue_wait_ms, witness_ms, groth16_ms, verify_ms, "groth16 prove ok"
        );
        Ok(proof)
    }
}

/// Witness to circom signal map, in the shape `tree_update_batch.circom`
/// declares. Kept separate from proving so the mapping is checkable without a
/// zkey; a wrong length would otherwise surface from circom as an opaque
/// witness-build failure.
fn circom_inputs(w: &TreeUpdateBatchWitness) -> AppResult<witness_calc::Inputs> {
    let mut inputs = witness_calc::Inputs::new();
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

/// Affine G1 in snarkjs proof shape: `[x, y, "1"]`, decimal.
fn g1_to_dec(p: &ark_bn254::G1Affine) -> [String; 3] {
    use ark_ec::AffineRepr;
    if p.is_zero() {
        return ["0".into(), "1".into(), "0".into()];
    }
    [fq_to_dec(&p.x), fq_to_dec(&p.y), "1".into()]
}

/// Affine G2 in snarkjs proof shape:
/// `[[x_c0, x_c1], [y_c0, y_c1], ["1", "0"]]`.
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

    /// A spend witness: `TRANSACT_OUT` leaves with no deposit binding, the shape
    /// both single-spend pipelines produce.
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

    /// The circuit declares fixed-width arrays, and a short or long signal is
    /// rejected inside circom with no useful message, so the widths are pinned
    /// here.
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

    /// Padding slots must be zero: the circuit and the contract both enforce it,
    /// and a stray value is otherwise invisible until the prove.
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
    //! `tree_update_batch.wcd`, `tree_update_batch_final.zkey` and a
    //! `vector.json` carrying the published vector. Writes `proof.json` and
    //! `public.json` next to them.

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
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
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
}
