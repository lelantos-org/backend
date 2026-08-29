//! Circom witness calculator, specialised for BN254.
//!
//! Evaluates the witness-calculation graph `just build-graph` emits
//! (`tree_update_batch.wcd`), rather than running circom's wasm generator. The
//! wasm path cost one `getWitness` call plus eight `readSharedRWMemory` calls
//! per signal — roughly 513k boundary crossings for a 57k-signal circuit — and
//! required a wasm runtime in the relayer. Graph evaluation is native Rust and
//! measured ~12x faster on `tree_update_batch`.
//!
//! The graph is built by `build-circuit` from the same circuit source at the
//! same optimisation level as the `.r1cs` the zkey was set up against, and the
//! recipe diffs the two constraint systems, so the signal ordering here matches
//! the ordering the zkey indexes.
//!
//! `circom-witnesscalc` exposes only `calc_witness`, which returns a serialised
//! `.wtns` file, so the witness makes a round trip through that encoding before
//! it reaches `Fr`. The parse below is the snarkjs `.wtns` reader; it is the
//! same format `sdk/wasm/prover/src/wtns.rs` reads on the browser side.
//!
//! Note that `calc_witness` prints two timing lines to stdout per call. That is
//! upstream behaviour with no feature flag to suppress it.

use crate::domain::error::{AppError, AppResult, ErrorContext};
use ark_bn254::Fr;
use ark_ff::{BigInteger256, PrimeField};
use byteorder::{LittleEndian, ReadBytesExt};
use circom_witnesscalc::calc_witness;
use num_bigint::BigInt;
use num_traits::Signed;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

/// `.wtns` magic, and the field width the parser accepts. BN254 only, as the
/// zkey is.
const MAGIC: &[u8; 4] = b"wtns";
const FIELD_BYTES: u32 = 32;

/// Signal inputs by circom name, as decimal-parsed integers.
pub type Inputs = HashMap<String, Vec<BigInt>>;

/// Computes witnesses for one circuit. Cheap to share: [`Self::calculate`] takes
/// `&self`, so callers need no lock around it.
pub struct WitnessCalculator {
    /// The serialised graph, held for the process lifetime. Deserialising it is
    /// ~4 ms and `calc_witness` does it per call, which is the remaining fixed
    /// cost of the upstream API.
    graph: Vec<u8>,
}

impl WitnessCalculator {
    /// Load the `.wcd` graph. Fails if the file is unreadable; a graph that is
    /// structurally invalid, or built for a different circuit, surfaces on the
    /// first [`Self::calculate`] instead.
    pub fn new(graph_path: &Path) -> AppResult<Self> {
        let graph = fs::read(graph_path).prover("read witness graph")?;
        Ok(Self { graph })
    }

    /// Run the circuit and return the full witness as field elements, in the
    /// signal order the zkey indexes against.
    ///
    /// Computing a witness does not check the constraints — a witness that
    /// violates them is returned rather than refused, exactly as the wasm
    /// generator behaved with `SANITY_CHECK` off. `Groth16Params::verify` is
    /// what catches it, before the proof becomes calldata.
    pub fn calculate(&self, inputs: Inputs) -> AppResult<Vec<Fr>> {
        let json = inputs_to_json(&inputs)?;
        let wtns = calc_witness(&json, &self.graph)
            .map_err(|e| AppError::Prover(format!("witness calculation failed: {e}")))?;
        parse_wtns(&wtns).map_err(|e| AppError::Prover(format!("parse calculated witness: {e}")))
    }
}

/// Signal map to the JSON object `calc_witness` parses.
///
/// Values go as decimal strings: the circuit's field elements do not fit a JSON
/// number, and upstream parses a string with `U256::from_str_radix`.
fn inputs_to_json(inputs: &Inputs) -> AppResult<String> {
    let mut map = Map::with_capacity(inputs.len());
    for (name, values) in inputs {
        let mut out = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            if !in_range(value) {
                return Err(AppError::Prover(format!(
                    "signal '{name}'[{index}] is out of field range"
                )));
            }
            out.push(Value::String(value.to_string()));
        }
        map.insert(name.clone(), Value::Array(out));
    }
    serde_json::to_string(&Value::Object(map))
        .map_err(|e| AppError::Prover(format!("encode witness inputs: {e}")))
}

/// Whether a signal value is representable.
///
/// Rejects a negative or over-wide value rather than letting it be truncated or
/// wrapped into a different signal value.
fn in_range(value: &BigInt) -> bool {
    !value.is_negative() && value.bits() <= 256
}

/// Parse a snarkjs `.wtns` buffer into field elements.
///
/// Format:
///   magic "wtns" (4) | version u32 | nSections u32
///   section 1 (header): n8 u32 | prime[n8] | nWitness u32
///   section 2 (witness): nWitness * n8 LE bytes
fn parse_wtns(bytes: &[u8]) -> Result<Vec<Fr>, String> {
    let mut cur = Cursor::new(bytes);

    let mut magic = [0u8; 4];
    cur.read_exact(&mut magic).map_err(io_err)?;
    if &magic != MAGIC {
        return Err("not a .wtns buffer".into());
    }
    let _version = read_u32(&mut cur)?;
    let n_sections = read_u32(&mut cur)?;

    let mut header: Option<Header> = None;
    let mut witness_off: Option<u64> = None;

    for _ in 0..n_sections {
        let id = read_u32(&mut cur)?;
        let size = read_u64(&mut cur)?;
        let start = cur.position();
        match id {
            1 => header = Some(read_header(&mut cur, start)?),
            2 => witness_off = Some(start),
            _ => {}
        }
        cur.set_position(start + size);
    }

    let Header { n8, n_witness } = header.ok_or("missing wtns section 1")?;
    let off = witness_off.ok_or("missing wtns section 2")?;
    if n8 != FIELD_BYTES {
        return Err(format!("expected n8={FIELD_BYTES}, got {n8}"));
    }

    cur.set_position(off);
    (0..n_witness).map(|i| read_fr(&mut cur, i)).collect()
}

struct Header {
    n8: u32,
    n_witness: u32,
}

fn read_header(cur: &mut Cursor<&[u8]>, sec_start: u64) -> Result<Header, String> {
    let n8 = read_u32(cur)?;
    // Skip the prime; the graph pins the field by its own header.
    cur.set_position(sec_start + 4 + u64::from(n8));
    let n_witness = read_u32(cur)?;
    Ok(Header { n8, n_witness })
}

/// One witness value, as plain little-endian limbs rather than Montgomery form,
/// so this repacks and lets `from_bigint` convert.
fn read_fr(cur: &mut Cursor<&[u8]>, index: u32) -> Result<Fr, String> {
    let mut limbs = [0u64; 4];
    for limb in limbs.iter_mut() {
        *limb = read_u64(cur)?;
    }
    Fr::from_bigint(BigInteger256::new(limbs))
        .ok_or_else(|| format!("witness[{index}] is not a canonical field element"))
}

fn read_u32(cur: &mut Cursor<&[u8]>) -> Result<u32, String> {
    cur.read_u32::<LittleEndian>().map_err(io_err)
}

fn read_u64(cur: &mut Cursor<&[u8]>) -> Result<u64, String> {
    cur.read_u64::<LittleEndian>().map_err(io_err)
}

fn io_err(e: std::io::Error) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A negative or over-wide input must be refused rather than truncated into
    /// a different signal value.
    #[test]
    fn out_of_range_inputs_are_rejected() {
        assert!(!in_range(&BigInt::from(-1i64)));
        assert!(!in_range(&(BigInt::from(1u8) << 256)));
    }

    /// The value where a naive width check would be wrong: the largest field
    /// element uses every limb and must still be accepted.
    #[test]
    fn the_largest_field_element_is_in_range() {
        let max = Fr::from(-1i64).into_bigint().to_string();
        assert!(in_range(&BigInt::parse_bytes(max.as_bytes(), 10).unwrap()));
    }

    /// Signal names and per-signal values must survive encoding: a dropped or
    /// renamed signal fails inside the graph with no useful message.
    #[test]
    fn inputs_encode_as_arrays_of_decimal_strings() {
        let inputs: Inputs = HashMap::from([
            ("z".to_string(), vec![BigInt::from(7u8)]),
            (
                "cms".to_string(),
                vec![BigInt::from(1u8), BigInt::from(0u8)],
            ),
        ]);
        let v: serde_json::Value = serde_json::from_str(&inputs_to_json(&inputs).unwrap()).unwrap();
        assert_eq!(v["z"], serde_json::json!(["7"]));
        assert_eq!(v["cms"], serde_json::json!(["1", "0"]));
    }

    #[test]
    fn a_non_wtns_buffer_is_refused() {
        assert!(parse_wtns(b"not a witness at all").is_err());
    }
}
