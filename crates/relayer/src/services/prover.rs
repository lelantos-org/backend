// `tree_update_batch` prover. In-process Groth16 over ark-bn254 against the
// snarkjs-compatible `.zkey`; the proving key is parsed once at startup.

use crate::domain::error::{AppError, AppResult};
use async_trait::async_trait;
use serde::Serialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tracing::info;

use ark_bn254::{Bn254, Fr};
use ark_circom::read_zkey;
use ark_circom::{CircomBuilder, CircomConfig, CircomReduction};
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
    /// `2 * MAX_N_BATCH` entries; padding (i ≥ 2*actual_count) MUST be "0".
    pub cms: Vec<String>,
    /// `2 * MAX_N_BATCH` Baby-Jubjub points (depositor-anchored value
    /// commitments). Padding entries MUST be "0".
    pub cv_dep: Vec<[String; 2]>,
    /// `MAX_N_BATCH` per-pair publicAssetId. Padding "0".
    pub pair_asset: Vec<String>,
    /// `MAX_N_BATCH` per-pair publicIn. Padding "0".
    pub pair_public_in: Vec<String>,
    /// `MAX_N_BATCH` 0/1 flags. 1 = deposit pair (aggregate enforced).
    pub is_deposit: Vec<String>,
    /// `MAX_N_BATCH` private rcv_dep sums. Padding "0".
    pub rcv_total: Vec<String>,
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

/// In-process Groth16 prover. Holds the parsed `ProvingKey` once; per call
/// it spawns a blocking task that builds a fresh `CircomConfig` (wasm +
/// r1cs are mmap-fast) and proves. Keeping pk Arc'd is the big win — zkey
/// parse is the dominant cost (~150MB zkey for tree_update_batch).
pub struct ArkCircomProver {
    pk: Arc<ProvingKey<Bn254>>,
    wasm_path: PathBuf,
    r1cs_path: PathBuf,
    /// Serializes proves; `Groth16::prove` is heavily multithreaded internally.
    gate: Arc<Mutex<()>>,
}

impl ArkCircomProver {
    pub fn new(wasm_path: &PathBuf, r1cs_path: &PathBuf, zkey_path: &PathBuf) -> AppResult<Self> {
        let _probe = CircomConfig::<Fr>::new(wasm_path, r1cs_path)
            .map_err(|e| AppError::Prover(format!("circom config probe: {}", e)))?;
        let mut zk = std::fs::File::open(zkey_path)
            .map_err(|e| AppError::Prover(format!("open zkey: {}", e)))?;
        let (pk, _matrices) =
            read_zkey(&mut zk).map_err(|e| AppError::Prover(format!("read zkey: {}", e)))?;
        Ok(Self {
            pk: Arc::new(pk),
            wasm_path: wasm_path.clone(),
            r1cs_path: r1cs_path.clone(),
            gate: Arc::new(Mutex::new(())),
        })
    }
}

#[async_trait]
impl TreeUpdateBatchProver for ArkCircomProver {
    async fn prove(&self, witness: TreeUpdateBatchWitness) -> AppResult<TreeUpdateBatchProof> {
        let pk = self.pk.clone();
        let wasm_path = self.wasm_path.clone();
        let r1cs_path = self.r1cs_path.clone();
        let gate = self.gate.clone();

        let started = Instant::now();
        info!(
            start_index = %witness.start_index,
            actual_count = %witness.actual_count,
            "ark-circom groth16 prove start"
        );

        let result = tokio::task::spawn_blocking(move || -> AppResult<TreeUpdateBatchProof> {
            let _guard = gate.lock();

            let mut cfg = CircomConfig::<Fr>::new(&wasm_path, &r1cs_path)
                .map_err(|e| AppError::Prover(format!("circom config: {}", e)))?;
            cfg.sanity_check = true;
            let mut builder = CircomBuilder::new(cfg);

            push_dec(&mut builder, "z", &witness.z)?;
            push_dec(&mut builder, "old_root", &witness.old_root)?;
            push_dec(&mut builder, "new_root", &witness.new_root)?;
            push_dec(&mut builder, "start_index", &witness.start_index)?;
            push_dec(&mut builder, "actual_count", &witness.actual_count)?;
            for cm in &witness.cms {
                push_dec(&mut builder, "cms", cm)?;
            }
            for pt in &witness.cv_dep {
                for c in pt {
                    push_dec(&mut builder, "cv_dep", c)?;
                }
            }
            for v in &witness.pair_asset {
                push_dec(&mut builder, "pair_asset", v)?;
            }
            for v in &witness.pair_public_in {
                push_dec(&mut builder, "pair_public_in", v)?;
            }
            for v in &witness.is_deposit {
                push_dec(&mut builder, "is_deposit", v)?;
            }
            for row in &witness.frontier_in {
                for cell in row {
                    push_dec(&mut builder, "frontier_in", cell)?;
                }
            }
            for v in &witness.rcv_total {
                push_dec(&mut builder, "rcv_total", v)?;
            }

            let circom = builder
                .build()
                .map_err(|e| AppError::Prover(format!("witness build: {}", e)))?;

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

fn push_dec(builder: &mut CircomBuilder<Fr>, name: &str, dec: &str) -> AppResult<()> {
    let bi = BigInt::from_str(dec)
        .map_err(|e| AppError::Prover(format!("input '{}' parse: {}", name, e)))?;
    builder.push_input(name, bi);
    Ok(())
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
