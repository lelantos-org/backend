//! Circom witness calculator, specialised for BN254.
//!
//! This is `ark_circom`'s wasm witness calculator with its two hot spots
//! removed. Upstream resolves every wasm export by name on *every* call —
//! `instance.exports.get_function("readSharedRWMemory")`, a string-keyed
//! hashmap lookup — and then invokes it through wasmer's dynamic path, which
//! boxes arguments and results. Extracting a 130k-signal witness costs one
//! `getWitness` plus eight `readSharedRWMemory` calls per signal, so that is
//! ~1.2M lookups and ~1.2M boxed calls per proof. It then rebuilds each field
//! element with eight `BigInt` multiply-adds and converts through `BigUint`,
//! for another ~2M heap allocations.
//!
//! Here the exports are resolved once into typed handles, and a signal goes
//! straight from its eight little-endian 32-bit words into `Fr`'s limbs.
//!
//! The wasm module, the calling sequence and the resulting witness are
//! unchanged — this produces the same field elements in the same order, which
//! is what the zkey's signal indexing requires.

use crate::domain::error::{AppError, AppResult, ErrorContext};
use ark_bn254::Fr;
use ark_ff::{BigInteger256, PrimeField};
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use std::collections::HashMap;
use std::path::Path;
use wasmer::{Instance, Memory, MemoryType, Module, RuntimeError, Store, TypedFunction, imports};

/// A field element as circom passes it through shared memory: 32-bit words,
/// least significant first.
const FIELD_WORDS: usize = 8;
type Words = [u32; FIELD_WORDS];

/// Linear memory the circom runtime expects, in 64 KiB pages (128 MiB).
const MEMORY_PAGES: u32 = 2000;

/// circom's own per-signal assignment checks, run inside the wasm on every
/// build. The prover verifies the finished proof instead, which covers the
/// same failure class against the whole witness at once and costs less.
const SANITY_CHECK: bool = false;

/// Signal inputs by circom name, as decimal-parsed integers.
pub type Inputs = HashMap<String, Vec<BigInt>>;

/// The circuit's exported entry points, resolved once at load.
struct Exports {
    init: TypedFunction<u32, ()>,
    get_field_num_len32: TypedFunction<(), u32>,
    get_raw_prime: TypedFunction<(), ()>,
    read_shared_rw_memory: TypedFunction<u32, u32>,
    write_shared_rw_memory: TypedFunction<(u32, u32), ()>,
    set_input_signal: TypedFunction<(u32, u32, u32), ()>,
    get_witness: TypedFunction<u32, ()>,
    get_witness_size: TypedFunction<(), u32>,
}

impl Exports {
    fn resolve(instance: &Instance, store: &Store) -> AppResult<Self> {
        macro_rules! export {
            ($name:literal) => {
                instance
                    .exports
                    .get_typed_function(store, $name)
                    .prover(concat!("witness wasm export ", $name))?
            };
        }
        Ok(Self {
            init: export!("init"),
            get_field_num_len32: export!("getFieldNumLen32"),
            get_raw_prime: export!("getRawPrime"),
            read_shared_rw_memory: export!("readSharedRWMemory"),
            write_shared_rw_memory: export!("writeSharedRWMemory"),
            set_input_signal: export!("setInputSignal"),
            get_witness: export!("getWitness"),
            get_witness_size: export!("getWitnessSize"),
        })
    }
}

pub struct WitnessCalculator {
    store: Store,
    exports: Exports,
}

impl WitnessCalculator {
    pub fn new(wasm_path: &Path) -> AppResult<Self> {
        let mut store = Store::default();
        let module = Module::from_file(&store, wasm_path).prover("load witness wasm")?;
        let memory = Memory::new(&mut store, MemoryType::new(MEMORY_PAGES, None, false))
            .prover("witness wasm memory")?;
        let imports = imports! {
            "env" => { "memory" => memory },
            "runtime" => {
                "error" => runtime::error(&mut store),
                "logSetSignal" => runtime::ignore_2(&mut store),
                "logGetSignal" => runtime::ignore_2(&mut store),
                "logFinishComponent" => runtime::ignore_1(&mut store),
                "logStartComponent" => runtime::ignore_1(&mut store),
                "log" => runtime::ignore_1(&mut store),
                "exceptionHandler" => runtime::ignore_1(&mut store),
                "showSharedRWMemory" => runtime::ignore_0(&mut store),
                "printErrorMessage" => runtime::ignore_0(&mut store),
                "writeBufferMessage" => runtime::ignore_0(&mut store),
            }
        };
        let instance =
            Instance::new(&mut store, &module, &imports).prover("instantiate witness wasm")?;

        let exports = Exports::resolve(&instance, &store)?;
        let mut this = Self { store, exports };
        this.check_field()?;
        Ok(this)
    }

    /// Run the circuit and return the full witness as field elements, in the
    /// signal order the zkey indexes against.
    pub fn calculate(&mut self, inputs: Inputs) -> AppResult<Vec<Fr>> {
        self.exports
            .init
            .call(&mut self.store, SANITY_CHECK as u32)
            .prover("witness init")?;
        self.write_inputs(inputs)?;
        self.read_witness()
    }

    fn write_inputs(&mut self, inputs: Inputs) -> AppResult<()> {
        for (name, values) in inputs {
            let (msb, lsb) = fnv1a(&name);
            for (index, value) in values.iter().enumerate() {
                let words = words_from_bigint(value).ok_or_else(|| {
                    AppError::Prover(format!("signal '{name}'[{index}] is out of field range"))
                })?;
                self.write_shared(&words)
                    .map_err(|e| AppError::Prover(format!("signal '{name}'[{index}]: {e}")))?;
                self.exports
                    .set_input_signal
                    .call(&mut self.store, msb, lsb, index as u32)
                    .map_err(|e| AppError::Prover(format!("set signal '{name}'[{index}]: {e}")))?;
            }
        }
        Ok(())
    }

    fn read_witness(&mut self) -> AppResult<Vec<Fr>> {
        let size = self
            .exports
            .get_witness_size
            .call(&mut self.store)
            .prover("getWitnessSize")?;

        let mut witness = Vec::with_capacity(size as usize);
        for i in 0..size {
            self.exports
                .get_witness
                .call(&mut self.store, i)
                .prover("getWitness")?;
            let words = self.read_shared().prover("readSharedRWMemory")?;
            witness.push(fr_from_words(&words).ok_or_else(|| {
                AppError::Prover(format!("witness[{i}] is not a canonical field element"))
            })?);
        }
        Ok(witness)
    }

    /// The zkey is BN254 and this module must agree, down to the modulus — a
    /// mismatched circuit would otherwise yield a witness that is silently
    /// wrong rather than one that fails to load.
    fn check_field(&mut self) -> AppResult<()> {
        let words = self
            .exports
            .get_field_num_len32
            .call(&mut self.store)
            .prover("getFieldNumLen32")? as usize;
        if words != FIELD_WORDS {
            return Err(AppError::Prover(format!(
                "witness wasm field is {words} words wide, expected {FIELD_WORDS}"
            )));
        }
        self.exports
            .get_raw_prime
            .call(&mut self.store)
            .prover("getRawPrime")?;
        // The modulus is not itself a reducible field element, so compare
        // limbs rather than going through `fr_from_words`.
        if limbs_from_words(&self.read_shared().prover("read prime")?) != Fr::MODULUS.0 {
            return Err(AppError::Prover(
                "witness wasm is not a BN254 circuit (field modulus mismatch)".into(),
            ));
        }
        Ok(())
    }

    /// Drain one field element from the wasm's shared scratch buffer. Whatever
    /// was asked for — a witness signal, the field modulus — lands here first.
    fn read_shared(&mut self) -> Result<Words, RuntimeError> {
        let mut words = [0u32; FIELD_WORDS];
        for (i, word) in words.iter_mut().enumerate() {
            *word = self
                .exports
                .read_shared_rw_memory
                .call(&mut self.store, i as u32)?;
        }
        Ok(words)
    }

    /// Stage one field element in the shared scratch buffer, ready for
    /// `setInputSignal` to consume.
    fn write_shared(&mut self, words: &Words) -> Result<(), RuntimeError> {
        for (i, word) in words.iter().enumerate() {
            self.exports
                .write_shared_rw_memory
                .call(&mut self.store, i as u32, *word)?;
        }
        Ok(())
    }
}

/// Eight little-endian 32-bit words → `Fr`.
///
/// Circom's shared memory holds the value in plain (non-Montgomery) form, so
/// this is a repack into `Fr`'s four 64-bit limbs plus the one Montgomery
/// conversion `from_bigint` does. `None` if the value is not reduced — which
/// the circuit should never produce.
fn fr_from_words(words: &Words) -> Option<Fr> {
    Fr::from_bigint(BigInteger256::new(limbs_from_words(words)))
}

fn limbs_from_words(w: &Words) -> [u64; 4] {
    [
        (w[0] as u64) | ((w[1] as u64) << 32),
        (w[2] as u64) | ((w[3] as u64) << 32),
        (w[4] as u64) | ((w[5] as u64) << 32),
        (w[6] as u64) | ((w[7] as u64) << 32),
    ]
}

/// `BigInt` → eight little-endian 32-bit words. `None` for a negative value or
/// one too wide for the field, both of which would otherwise be truncated into
/// a silently different signal.
fn words_from_bigint(value: &BigInt) -> Option<Words> {
    let mut words = [0u32; FIELD_WORDS];
    let radix = BigInt::from(1u64 << 32);
    let mut rem = value.clone();
    for word in words.iter_mut() {
        if rem.is_zero() {
            return Some(words);
        }
        // A negative input makes the remainder negative, and `to_u32` rejects
        // it — so this doubles as the sign check.
        *word = (&rem % &radix).to_u32()?;
        rem /= &radix;
    }
    rem.is_zero().then_some(words)
}

/// FNV-1a, split high/low — how circom addresses an input signal by name.
fn fnv1a(name: &str) -> (u32, u32) {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = name.as_bytes().iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ *byte as u64).wrapping_mul(PRIME)
    });
    ((hash >> 32) as u32, hash as u32)
}

/// Host functions the circom runtime imports. All but `error` are debug hooks
/// the circuit calls and we ignore; they still have to be supplied, or the
/// module will not instantiate.
mod runtime {
    use wasmer::{Function, RuntimeError, Store};

    /// A failed assert inside the circuit. Trapping here is what turns it into
    /// an `Err` out of `calculate` rather than a silently wrong witness.
    pub fn error(store: &mut Store) -> Function {
        Function::new_typed(
            store,
            |a: i32, b: i32, c: i32, d: i32, e: i32, f: i32| -> Result<(), RuntimeError> {
                Err(RuntimeError::new(format!(
                    "circuit assert failed: {a} {b} {c} {d} {e} {f}"
                )))
            },
        )
    }

    pub fn ignore_0(store: &mut Store) -> Function {
        Function::new_typed(store, || {})
    }

    pub fn ignore_1(store: &mut Store) -> Function {
        Function::new_typed(store, |_: i32| {})
    }

    pub fn ignore_2(store: &mut Store) -> Function {
        Function::new_typed(store, |_: i32, _: i32| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_round_trip_through_fr() {
        for v in [0u64, 1, 42, u32::MAX as u64, u64::MAX] {
            let words = words_from_bigint(&BigInt::from(v)).unwrap();
            assert_eq!(fr_from_words(&words), Some(Fr::from(v)), "{v}");
        }
    }

    /// The value where a naive limb repack would be off: the modulus minus one
    /// uses every limb.
    #[test]
    fn the_largest_field_element_round_trips() {
        let max = Fr::from(-1i64);
        let decimal = max.into_bigint().to_string();
        let words = words_from_bigint(&BigInt::parse_bytes(decimal.as_bytes(), 10).unwrap());
        assert_eq!(fr_from_words(&words.unwrap()), Some(max));
    }

    /// A negative or over-wide input must be refused, not truncated into a
    /// different signal value.
    #[test]
    fn out_of_range_inputs_are_rejected() {
        assert_eq!(words_from_bigint(&BigInt::from(-1i64)), None);
        assert_eq!(words_from_bigint(&(BigInt::from(1u8) << 256)), None);
    }

    /// Pinned against reference FNV-1a values for signal names the circuit
    /// declares. A wrong hash addresses the wrong input signal.
    #[test]
    fn fnv1a_matches_reference_values() {
        for (name, msb, lsb) in [
            ("z", 2942564172u32, 2248284781u32),
            ("old_root", 1335788253, 4075583591),
            ("cms", 4127984665, 218607802),
            ("cv_dep", 1097964397, 1093556690),
            ("frontier_in", 2348300664, 2190871622),
            ("rcv", 2315104281, 1627738972),
        ] {
            assert_eq!(fnv1a(name), (msb, lsb), "{name}");
        }
    }
}
