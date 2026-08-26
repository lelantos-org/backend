//! Poseidon hashing, circomlib-compatible.
//!
//! The permutation is implemented here (see [`circom`]) over circomlib's
//! constants. Every hasher is built once per arity per thread and reused, since
//! construction parses the round constants and the MDS matrix.

mod circom;
mod sparse;

use ark_ed_on_bn254::Fq;
use ark_ff::{BigInteger, PrimeField};
use circom::{Circom, MAX_WIDTH};
use std::cell::RefCell;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PoseidonError {
    #[error("unsupported arity: {0}")]
    UnsupportedArity(usize),
    #[error("expected {expected} inputs, got {got}")]
    WrongInputCount { got: usize, expected: usize },
    #[error("input is not smaller than the field modulus")]
    InputLargerThanModulus,
    #[error("poseidon parameters: {0}")]
    Params(String),
}

thread_local! {
    /// One hasher per arity, indexed by arity so lookup is a bounds check.
    /// Thread-local because the filter hashes under rayon.
    static HASHERS: RefCell<Vec<Option<Circom>>> = const { RefCell::new(Vec::new()) };
}

/// Run `f` against this thread's hasher for `arity`, building it on first use.
fn with_hasher<T>(
    arity: usize,
    f: impl FnOnce(&Circom) -> Result<T, PoseidonError>,
) -> Result<T, PoseidonError> {
    HASHERS.with(|cell| {
        let mut slots = cell.borrow_mut();
        if slots.len() <= arity {
            slots.resize_with(arity + 1, || None);
        }
        if slots[arity].is_none() {
            slots[arity] = Some(Circom::new(arity)?);
        }
        f(slots[arity].as_ref().expect("initialised above"))
    })
}

pub fn hash(inputs: &[Fq]) -> Result<Fq, PoseidonError> {
    with_hasher(inputs.len(), |h| h.hash(inputs))
}

/// Big-endian bytes in, big-endian digest out.
///
/// Each input must be a canonical field element. One that is not smaller than
/// the modulus is rejected rather than reduced, so two distinct byte strings
/// cannot collide by wrapping.
pub fn hash_bytes_be(inputs: &[&[u8]]) -> Result<[u8; 32], PoseidonError> {
    if inputs.len() + 1 > MAX_WIDTH {
        return Err(PoseidonError::UnsupportedArity(inputs.len()));
    }

    let mut felts = [Fq::default(); MAX_WIDTH];
    for (slot, bytes) in felts.iter_mut().zip(inputs) {
        *slot = fq_from_be_canonical(bytes)?;
    }

    let out = with_hasher(inputs.len(), |h| h.hash(&felts[..inputs.len()]))?;

    let mut buf = [0u8; 32];
    let be = out.into_bigint().to_bytes_be();
    buf[32 - be.len()..].copy_from_slice(&be);
    Ok(buf)
}

fn fq_from_be_canonical(bytes: &[u8]) -> Result<Fq, PoseidonError> {
    let n = num_bigint::BigUint::from_bytes_be(bytes);
    let bigint = <Fq as PrimeField>::BigInt::try_from(n)
        .map_err(|_| PoseidonError::InputLargerThanModulus)?;
    if bigint >= Fq::MODULUS {
        return Err(PoseidonError::InputLargerThanModulus);
    }
    Fq::from_bigint(bigint).ok_or(PoseidonError::InputLargerThanModulus)
}

#[cfg(test)]
mod tests;
