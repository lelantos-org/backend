//! FMD bit derivation + clue test.
//!
//! Bit derivation (v3, SNARK-friendly):
//!   `bit_i = legendre(Poseidon([TAG_FMD_BIT, R.x, R.y, i, S_i.x, S_i.y]))`
//! where `legendre(h) = 1` iff `h` is a quadratic residue in 𝔽_r (BN254 scalar).
//! Receiver accepts iff for all i ∈ [γ], `bit_i ⊕ c_bits[i] == 1`.
//!
//! The Legendre symbol is used instead of `lsb1` extraction so the in-circuit
//! `HashToBit` gadget verifies in 4 constraints rather than the ~254 that
//! `Num2Bits` requires.

use crate::poseidon::hash as poseidon_hash;
use ark_ed_on_bn254::{Fq, Fr};
use ark_ff::{Field, LegendreSymbol};

use super::coords::{CircomPoint, FixedBaseTable, scalar_mul};

/// Domain-separation tag for FMD bit derivation, mirroring `TAG_FMD_BIT` in
/// `circuits/src/lib/tags.circom`. Must not collide with the other Poseidon tags
/// in this codebase, which occupy 1..=7.
pub const TAG_FMD_BIT: u64 = 8;

/// Compute the per-component `bit_i` for a given clue R + shared secret S_i.
///
/// Inputs are circomlib Baby-Jubjub coordinates. The in-circuit `ClueCheck`
/// template runs the same Poseidon over the same six field elements, so witness
/// derivation matches receiver-side detection byte for byte.
fn shared_bit(r: &CircomPoint, i: u32, s: &CircomPoint) -> u8 {
    let inputs = [
        Fq::from(TAG_FMD_BIT),
        r.x,
        r.y,
        Fq::from(u64::from(i)),
        s.x,
        s.y,
    ];
    let h = poseidon_hash(&inputs).expect("poseidon arity 6 supported");
    match h.legendre() {
        LegendreSymbol::QuadraticResidue => 1,
        LegendreSymbol::QuadraticNonResidue => 0,
        // h == 0 has probability 1/r ≈ 2^-254. Treated as bit 0 to keep the
        // function total; the SNARK gadget rejects hash = 0 explicitly.
        LegendreSymbol::Zero => 0,
    }
}

/// Test a clue against a detection key.
///
/// `r` is the clue's randomness commitment R (a Baby-Jubjub point in circomlib
/// coordinates). `clue_bits` packs `gamma` bits LSB-first in byte-major order.
/// `dk` is the per-component detection-key scalar list of length `gamma`.
pub fn test_clue(dk: &[Fr], r: CircomPoint, clue_bits: u16, gamma: usize) -> bool {
    if dk.len() != gamma || gamma > 16 {
        return false;
    }
    if !r.is_on_curve() {
        return false;
    }
    for i in 0..(gamma as u32) {
        let s = scalar_mul(r, dk[i as usize]);
        let bit = shared_bit(&r, i, &s);
        let c_bit = ((clue_bits >> i) & 1) as u8;
        if (bit ^ c_bit) != 1 {
            return false;
        }
    }
    true
}

/// Test one clue against many detection keys at once.
///
/// Returns a `Vec<bool>` of length `dks.len()`, aligned with the input order.
/// Equivalent to calling [`test_clue`] for each `dk` with the same `(r,
/// clue_bits, gamma)`, but amortizes a fixed-base window table over all keys and
/// culls survivors bit by bit so each subsequent batch shrinks.
pub fn test_clue_batch(dks: &[&[Fr]], r: CircomPoint, clue_bits: u16, gamma: usize) -> Vec<bool> {
    let n = dks.len();
    if n == 0 {
        return Vec::new();
    }
    if gamma > 16 || !r.is_on_curve() {
        return vec![false; n];
    }
    let mut result = vec![true; n];
    let mut alive: Vec<usize> = Vec::with_capacity(n);
    for (j, dk) in dks.iter().enumerate() {
        if dk.len() == gamma {
            alive.push(j);
        } else {
            result[j] = false;
        }
    }
    if alive.is_empty() {
        return result;
    }

    let table = FixedBaseTable::new(r, alive.len());
    let mut scalars: Vec<Fr> = Vec::with_capacity(alive.len());
    for i in 0..(gamma as u32) {
        if alive.is_empty() {
            break;
        }
        scalars.clear();
        scalars.extend(alive.iter().map(|&j| dks[j][i as usize]));
        let products = table.batch_mul(&scalars);
        let c_bit = ((clue_bits >> i) & 1) as u8;
        let mut survivors = Vec::with_capacity(alive.len());
        for (idx, &j) in alive.iter().enumerate() {
            let bit = shared_bit(&r, i, &products[idx]);
            if (bit ^ c_bit) == 1 {
                survivors.push(j);
            } else {
                result[j] = false;
            }
        }
        alive = survivors;
    }
    result
}
