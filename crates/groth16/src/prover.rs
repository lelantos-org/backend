//! `tree_update_batch` prover: in-process Groth16 over ark-bn254 against the
//! snarkjs-compatible `.zkey`, with the proving key parsed once at startup.

use crate::error::{ErrorContext, Groth16Error, Groth16Result};
use crate::witness_calc::{self, WitnessCalculator};
use async_trait::async_trait;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::info;

use crate::zkey::read_zkey;
use ark_bn254::{Bn254, Fr};
use ark_ff::{PrimeField, UniformRand};
use ark_groth16::{Groth16, PreparedVerifyingKey, Proof, ProvingKey};
use ark_snark::SNARK;
use num_bigint::BigInt;
use taceo_groth16::{
    CircomReduction as TaceoCircomReduction, ConstraintMatrices, Groth16 as TaceoGroth16,
};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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
    ) -> Groth16Result<TreeUpdateBatchProof>;

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
    /// The A/B/C matrices plus the shape counts the prover checks the witness
    /// against.
    matrices: ConstraintMatrices<Fr>,
    /// Verifies the proof just produced, in place of circom's per-signal sanity
    /// check: three pairings against a whole-witness re-check.
    pvk: PreparedVerifyingKey<Bn254>,
    /// Length of the public witness prefix: the leading `1` plus the circuit's
    /// public outputs and inputs.
    num_inputs: usize,
    num_constraints: usize,
}

impl Groth16Params {
    fn load(zkey_path: &Path) -> Groth16Result<Self> {
        let mut file = std::fs::File::open(zkey_path).prover("open zkey")?;
        let (pk, m) = read_zkey(&mut file).prover("read zkey")?;
        let pvk = Groth16::<Bn254>::process_vk(&pk.vk).prover("process vk")?;
        Ok(Self {
            // The non-zero counts are the one part of this struct the prover
            // never reads; only the length check uses the variable counts.
            matrices: ConstraintMatrices {
                num_instance_variables: m.num_instance_variables,
                num_witness_variables: m.num_witness_variables,
                num_constraints: m.num_constraints,
                a_num_non_zero: 0,
                b_num_non_zero: 0,
                c_num_non_zero: 0,
                a: m.a,
                b: m.b,
                c: m.c,
            },
            pk,
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
    fn public_inputs<'w>(&self, witness: &'w [Fr]) -> Groth16Result<&'w [Fr]> {
        witness
            .get(1..self.num_inputs)
            .ok_or_else(|| Groth16Error::Prove("witness shorter than its public inputs".into()))
    }

    fn prove(&self, witness: &[Fr]) -> Groth16Result<Proof<Bn254>> {
        let mut rng = rand::rngs::OsRng;
        let (r, s) = (Fr::rand(&mut rng), Fr::rand(&mut rng));
        TaceoGroth16::<Bn254>::prove::<TaceoCircomReduction>(
            &self.pk,
            r,
            s,
            &self.matrices,
            witness,
        )
        .map_err(|e| Groth16Error::Prove(format!("groth16 prove: {e}")))
    }

    /// Check our own output before it becomes calldata.
    ///
    /// Stands in for circom's `sanity_check`: a witness that does not satisfy the
    /// constraints cannot yield a verifying proof, and failing here is a failed
    /// submission rather than an opaque on-chain revert.
    fn verify(&self, public_inputs: &[Fr], proof: &Proof<Bn254>) -> Groth16Result<()> {
        let ok = Groth16::<Bn254>::verify_with_processed_vk(&self.pvk, public_inputs, proof)
            .prover("verify own proof")?;
        if !ok {
            return Err(Groth16Error::Prove("own proof failed verification".into()));
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

/// In-process Groth16 prover. The proving key, a ~50 MB zkey, the A/B/C matrices
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
    pub fn new(graph_path: &Path, zkey_path: &Path) -> Groth16Result<Self> {
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
    async fn acquire(&self, priority: Priority) -> Groth16Result<SemaphorePermit<'_>> {
        match priority {
            Priority::Spend => self.gate.acquire().await.prover("prove gate"),
            Priority::Flush => self.gate.try_acquire().map_err(|_| Groth16Error::Busy),
        }
    }
}

/// Witness to proof, start to finish. Blocking and CPU-bound; the caller runs it
/// on a blocking thread while holding the prover's permit.
fn run_proof(
    params: &Groth16Params,
    wtns: &WitnessCalculator,
    inputs: witness_calc::Inputs,
) -> Groth16Result<(TreeUpdateBatchProof, Timings)> {
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
    ) -> Groth16Result<TreeUpdateBatchProof> {
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

impl TreeUpdateBatchWitness {
    /// The circuit's signals, in the shape `tree_update_batch.circom` declares:
    /// name to flattened decimal values, arrays flattened and scalars a single
    /// element.
    ///
    /// Public, and separate from proving, so whoever builds a witness can pin
    /// the widths and the padding without a zkey and without naming this crate's
    /// arithmetic types. A signal of the wrong length would otherwise surface
    /// from circom as an opaque witness-build failure.
    ///
    /// Borrows: this is a view, for inspection and for the prover's own parse
    /// into field elements, and both are done with it before the witness is.
    pub fn signals(&self) -> Vec<(&'static str, Vec<&str>)> {
        /// Flattening a point or a frontier row is what turns it into the width
        /// circom expects.
        fn flat<const N: usize>(rows: &[[String; N]]) -> Vec<&str> {
            rows.iter().flatten().map(String::as_str).collect()
        }
        fn each(values: &[String]) -> Vec<&str> {
            values.iter().map(String::as_str).collect()
        }
        fn one(value: &str) -> Vec<&str> {
            vec![value]
        }

        vec![
            ("z", one(&self.z)),
            ("old_root", one(&self.old_root)),
            ("new_root", one(&self.new_root)),
            ("start_index", one(&self.start_index)),
            ("actual_count", one(&self.actual_count)),
            ("cms", each(&self.cms)),
            ("cv_dep", flat(&self.cv_dep)),
            ("leaf_asset", each(&self.leaf_asset)),
            ("leaf_public_in", each(&self.leaf_public_in)),
            ("is_deposit", each(&self.is_deposit)),
            ("rcv", each(&self.rcv)),
            ("frontier_in", flat(&self.frontier_in)),
        ]
    }
}

/// Parse [`TreeUpdateBatchWitness::signals`] into the integers the witness
/// graph consumes.
fn circom_inputs(w: &TreeUpdateBatchWitness) -> Groth16Result<witness_calc::Inputs> {
    let signals = w.signals();
    let mut inputs = witness_calc::Inputs::with_capacity(signals.len());
    for (name, decimals) in signals {
        let values = decimals
            .iter()
            .map(|d| {
                BigInt::from_str(d)
                    .map_err(|e| Groth16Error::Prove(format!("signal '{name}' value '{d}': {e}")))
            })
            .collect::<Groth16Result<Vec<_>>>()?;
        // Guards a duplicate in the table above, which would otherwise silently
        // drop whichever entry lost.
        if inputs.insert(name.to_string(), values).is_some() {
            return Err(Groth16Error::Prove(format!("signal '{name}' set twice")));
        }
    }
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
    use crate::qap::CircomReduction;

    /// A witness with `n` leaf slots and a depth-`n` frontier. The real widths
    /// come from the circuit and are pinned where the witness is built; what is
    /// checked here is the mapping itself, which is this crate's.
    fn witness(n: usize) -> TreeUpdateBatchWitness {
        TreeUpdateBatchWitness {
            z: "1".into(),
            old_root: "2".into(),
            new_root: "3".into(),
            start_index: "4".into(),
            actual_count: "1".into(),
            cms: vec!["5".into(); n],
            cv_dep: vec![["6".into(), "7".into()]; n],
            leaf_asset: vec!["0".into(); n],
            leaf_public_in: vec!["0".into(); n],
            is_deposit: vec!["0".into(); n],
            rcv: vec!["0".into(); n],
            frontier_in: vec![["8".into(), "9".into(), "10".into()]; n],
        }
    }

    /// Every signal the circuit declares is supplied, and nothing else is: an
    /// unknown name and a missing one both fail inside the graph with no useful
    /// message.
    #[test]
    fn every_declared_signal_is_supplied_and_no_others() {
        let w = witness(2);
        let mut names: Vec<&str> = w.signals().into_iter().map(|(n, _)| n).collect();
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

    /// A duplicate in the signal table would silently drop whichever entry lost,
    /// and the circuit would be proved against a witness nobody wrote.
    #[test]
    fn no_signal_is_declared_twice() {
        let w = witness(2);
        let names: Vec<&str> = w.signals().into_iter().map(|(n, _)| n).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "duplicate signal in the table");
    }

    /// Points and frontier rows flatten; scalars stay single. A signal that
    /// arrived nested would be the wrong width for the circuit's declaration.
    #[test]
    fn array_signals_flatten_and_scalars_do_not() {
        let w = witness(2);
        let signals = w.signals();
        let width = |name: &str| {
            signals
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .1
                .len()
        };
        for scalar in ["z", "old_root", "new_root", "start_index", "actual_count"] {
            assert_eq!(width(scalar), 1, "{scalar}");
        }
        assert_eq!(width("cms"), 2);
        assert_eq!(width("cv_dep"), 4, "flattened BJJ points");
        assert_eq!(
            width("frontier_in"),
            6,
            "flattened depth rows of 3 siblings"
        );
    }

    /// The values must reach the graph as the integers the circuit sees, in
    /// order: a reordered or reinterpreted signal proves the wrong statement.
    #[test]
    fn values_are_parsed_as_decimal_in_order() {
        let inputs = circom_inputs(&witness(1)).unwrap();
        assert_eq!(inputs["z"], vec![BigInt::from(1u8)]);
        assert_eq!(inputs["cv_dep"], vec![BigInt::from(6u8), BigInt::from(7u8)]);
        assert_eq!(
            inputs["frontier_in"],
            vec![BigInt::from(8u8), BigInt::from(9u8), BigInt::from(10u8)]
        );
    }

    #[test]
    fn a_non_numeric_signal_names_itself_in_the_error() {
        let mut w = witness(1);
        w.cms[0] = "not-a-number".into();
        let err = circom_inputs(&w).unwrap_err();
        assert!(err.to_string().contains("cms"), "got {err}");
    }

    /// Where one proof spends its time, and whether `taceo-groth16` computes the
    /// same proof faster.
    ///
    /// Skipped unless `ZKEY_COMPAT_DIR` holds `tree_update_batch.wcd` and
    /// `tree_update_batch_final.zkey` — `circuits/build` is one such directory.
    /// Unlike `tests/zkey_compat.rs` this needs no published vector: proving cost
    /// is set by the circuit's shape, not by whether the witness satisfies it, so
    /// an arbitrary witness of the right widths times the same.
    ///
    /// Correctness is still pinned, and more tightly than a verification against
    /// an unsatisfying witness could manage: a Groth16 proof is a deterministic
    /// function of `(pk, r, s, witness)`, so holding `r` and `s` fixed, the two
    /// provers must agree on every coordinate. They compute the same thing by
    /// different routes, which is exactly what needs checking before one replaces
    /// the other.
    ///
    /// Run with `--features print-trace` to add ark-groth16's own per-MSM timers.
    ///
    /// ```text
    /// ZKEY_COMPAT_DIR=../../../circuits/build \\
    ///   cargo test -p groth16 --release --features print-trace \\
    ///   -- --ignored --nocapture measure_phase_split
    /// ```
    #[test]
    #[ignore = "needs a 48 MB zkey; run explicitly"]
    fn measure_phase_split() {
        let Ok(dir) = std::env::var("ZKEY_COMPAT_DIR") else {
            eprintln!("ZKEY_COMPAT_DIR unset; skipping");
            return;
        };
        let dir = Path::new(&dir);

        // The widths this artifact declares. `circuits/src/tree_update_batch.circom`
        // instantiates `TreeUpdateBatch(11, 8)`, so `frontier_in` is `DEPTH` = 11
        // rows, not the 10 the relayer pins for its shallower deployment tree.
        // A wrong width fails inside the graph, not here.
        let w = TreeUpdateBatchWitness {
            z: "1".into(),
            old_root: "2".into(),
            new_root: "3".into(),
            start_index: "0".into(),
            actual_count: "0".into(),
            cms: vec!["0".into(); 8],
            cv_dep: vec![["0".into(), "0".into()]; 8],
            leaf_asset: vec!["0".into(); 8],
            leaf_public_in: vec!["0".into(); 8],
            is_deposit: vec!["0".into(); 8],
            rcv: vec!["0".into(); 8],
            frontier_in: vec![["0".into(), "0".into(), "0".into()]; 11],
        };

        let mut load_ms = 0;
        let (wtns, params) = timed(&mut load_ms, || {
            (
                WitnessCalculator::new(&dir.join("tree_update_batch.wcd")).expect("graph"),
                Groth16Params::load(&dir.join("tree_update_batch_final.zkey")).expect("zkey"),
            )
        });

        let mut witness_ms = 0;
        let witness = timed(&mut witness_ms, || {
            wtns.calculate(circom_inputs(&w).expect("inputs"))
                .expect("witness")
        });

        // Fixed rather than sampled, so the two provers are comparable at all.
        let (r, s) = (Fr::from(7u64), Fr::from(11u64));

        // The oracle: ark-groth16 driving this crate's own vendored reduction,
        // which is the shape `create_proof_with_reduction_and_matrices` indexes.
        let ark_matrices = vec![
            params.matrices.a.clone(),
            params.matrices.b.clone(),
            params.matrices.c.clone(),
        ];
        let mut ark_ms = 0;
        let ark_proof = timed(&mut ark_ms, || {
            Groth16::<Bn254, CircomReduction>::create_proof_with_reduction_and_matrices(
                &params.pk,
                r,
                s,
                &ark_matrices,
                params.num_inputs,
                params.num_constraints,
                &witness,
            )
            .expect("ark prove")
        });

        // `prove` takes the witness length straight from the zkey and rejects a
        // witness that disagrees. The checked-in `circuits/build` artifacts are
        // from three different builds — the `.wcd` predates the `.zkey` by ten
        // days — so the graph here emits a witness a few elements longer than
        // this zkey declares. Both provers ignore the excess. The override keeps
        // that local staleness from masquerading as a prover bug; it is
        // deliberately not what the shipped path does.
        let matrices = ConstraintMatrices {
            num_witness_variables: witness.len() - params.num_inputs,
            ..params.matrices.clone()
        };
        let mut taceo_ms = 0;
        let taceo_proof = timed(&mut taceo_ms, || {
            TaceoGroth16::<Bn254>::prove::<TaceoCircomReduction>(
                &params.pk, r, s, &matrices, &witness,
            )
            .expect("taceo prove")
        });

        eprintln!(
            "[phase-split] threads={} constraints={} load={load_ms}ms witness={witness_ms}ms\n\
             [phase-split] ark-groth16   = {ark_ms}ms\n\
             [phase-split] taceo-groth16 = {taceo_ms}ms  ({:.2}x)",
            rayon::current_num_threads(),
            params.num_constraints,
            ark_ms as f64 / taceo_ms.max(1) as f64,
        );

        assert_eq!(ark_proof.a, taceo_proof.a, "pi_a differs");
        assert_eq!(ark_proof.b, taceo_proof.b, "pi_b differs");
        assert_eq!(ark_proof.c, taceo_proof.c, "pi_c differs");
    }

    /// The same comparison against an arbitrary zkey, to separate "this circuit"
    /// from "this platform" when a browser measurement disagrees with a native
    /// one.
    ///
    /// The witness is synthetic — this circuit has no graph here — so the
    /// absolute numbers mean nothing. Only the ratio does, and only as a
    /// same-circuit control: a real circom witness is mostly zeros and ones,
    /// which both provers have fast paths for, so a synthetic one shifts work
    /// into the full-width path for *both* of them.
    ///
    /// ```text
    /// ZKEY=../../../circuits/build/4x6_final.zkey \\
    ///   cargo test -p groth16 --release -- --ignored --nocapture compare_on_zkey
    /// ```
    #[test]
    #[ignore = "needs a zkey; run explicitly"]
    fn compare_on_zkey() {
        let Ok(path) = std::env::var("ZKEY") else {
            eprintln!("ZKEY unset; skipping");
            return;
        };
        let params = Groth16Params::load(Path::new(&path)).expect("zkey");
        let n = params.num_inputs + params.matrices.num_witness_variables;

        // Deterministic, and a mix of widths rather than all-full-width: every
        // fourth entry is small, which is the shape the scalar-size split in
        // `ark-ec`'s MSM keys off.
        let witness: Vec<Fr> = (0..n)
            .map(|i| match i % 4 {
                0 => Fr::from(0u64),
                1 => Fr::from(1u64),
                2 => Fr::from((i as u64) % 251),
                _ => Fr::from(i as u64) * Fr::from(7_919_u64),
            })
            .collect();

        let (r, s) = (Fr::from(7u64), Fr::from(11u64));
        let ark_matrices = vec![
            params.matrices.a.clone(),
            params.matrices.b.clone(),
            params.matrices.c.clone(),
        ];

        let mut ark_ms = 0;
        let ark_proof = timed(&mut ark_ms, || {
            Groth16::<Bn254, CircomReduction>::create_proof_with_reduction_and_matrices(
                &params.pk,
                r,
                s,
                &ark_matrices,
                params.num_inputs,
                params.num_constraints,
                &witness,
            )
            .expect("ark prove")
        });
        let mut taceo_ms = 0;
        let taceo_proof = timed(&mut taceo_ms, || {
            TaceoGroth16::<Bn254>::prove::<TaceoCircomReduction>(
                &params.pk,
                r,
                s,
                &params.matrices,
                &witness,
            )
            .expect("taceo prove")
        });

        eprintln!(
            "[compare] {path}\n\
             [compare] threads={} vars={n} constraints={}\n\
             [compare] ark-groth16   = {ark_ms}ms\n\
             [compare] taceo-groth16 = {taceo_ms}ms  ({:.2}x)",
            rayon::current_num_threads(),
            params.num_constraints,
            ark_ms as f64 / taceo_ms.max(1) as f64,
        );
        assert_eq!(ark_proof.a, taceo_proof.a, "pi_a differs");
        assert_eq!(ark_proof.b, taceo_proof.b, "pi_b differs");
        assert_eq!(ark_proof.c, taceo_proof.c, "pi_c differs");
    }
}
