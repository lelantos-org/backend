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
use super::fq_to_scalar;

/// Domain-separation tag for FMD bit derivation, mirroring `TAG_FMD_BIT` in
/// `circuits/src/lib/tags.circom`. Must not collide with the other Poseidon tags
/// in this codebase, which occupy 1..=7.
pub const TAG_FMD_BIT: u64 = 8;

/// Tag of the public expansion constants; see [`detection_key`]. Mirrors
/// `sdk/src/crypto/tags.ts`.
pub const TAG_FMD_EXPAND2: u64 = 17;

/// Clue bits every sender emits. Mirrors `FMD_DEFAULT_GAMMA` in
/// `sdk/src/fmd/keys.ts`.
pub const GAMMA: usize = 5;

/// Compute the per-component `bit_i` for a given clue R + shared secret S_i.
///
/// Inputs are circomlib Baby-Jubjub coordinates. The sender SDK runs the same
/// Poseidon over the same six field elements off-circuit, so flagging matches
/// detection bit for bit.
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

/// Whether a clue's `R` may be multiplied by a detection key.
///
/// On-curve is not enough. Baby-Jubjub's group is `Z_8 x Z_n`, so a sender can
/// pick `R = T + [t]B8` with `T` in the 8-torsion; `[dk_i]R` then has only eight
/// possible torsion parts, so `bit_i` depends on `dk_i mod 8` and a caller who
/// can see whether a crafted clue matched learns three bits of the key per
/// component. `R` reaches here straight off a chain event.
///
/// [`crate::note::try_decrypt`] faces the same attack on `epk` and clears the
/// cofactor instead. That is not available here: the clue bits are defined over
/// `[dk_i]R` itself, so `R` has to be a prime-order point rather than merely
/// reducible to one.
///
/// The identity is refused for the reason `unpack_subgroup` refuses it: it
/// absorbs every scalar, so `S_i` is the same for every key and the bits stop
/// depending on the detection key at all. One clue built that way matches every
/// subscriber.
pub fn usable_as_clue(r: &CircomPoint) -> bool {
    r.is_on_curve() && !r.is_identity() && r.is_in_prime_subgroup()
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
    if !usable_as_clue(&r) {
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

/// The [`GAMMA`] detection scalars of a root secret:
/// `x_i = dk_root + h_i`, `h_i = Poseidon(TAG_FMD_EXPAND2, i) mod q`. Mirrors
/// `fmdDiversifiedDetectionKey` in `sdk/src/fmd/diversified.ts`.
///
/// Independent of the diversifier: one key tests the clues of every address of
/// `dk_root`.
pub fn detection_key(dk_root: Fr) -> Vec<Fr> {
    (0..GAMMA as u64)
        .map(|i| {
            let h = poseidon_hash(&[Fq::from(TAG_FMD_EXPAND2), Fq::from(i)])
                .expect("poseidon arity 2 supported");
            dk_root + fq_to_scalar(h)
        })
        .collect()
}

/// The clue a sender produces with blinder `r` for an address on base `g_d`,
/// computed from the owner's detection key: `R = r·g_d`, `S_i = x_i·R`,
/// `c_i = bit_i ⊕ 1`. Mirrors `fmdExpectedClueOnBase` in
/// `sdk/src/fmd/diversified.ts`.
///
/// Bit `i` of the result is `c_i`, the packing [`test_clue`] reads. `dk` holds
/// at most 16 scalars.
pub fn expected_clue(dk: &[Fr], g_d: CircomPoint, r: Fr) -> (CircomPoint, u16) {
    let clue_r = scalar_mul(g_d, r);
    let bits = dk.iter().enumerate().fold(0u16, |acc, (i, x)| {
        let c_bit = shared_bit(&clue_r, i as u32, &scalar_mul(clue_r, *x)) ^ 1;
        acc | (u16::from(c_bit) << i)
    });
    (clue_r, bits)
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
    if gamma > 16 || !usable_as_clue(&r) {
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
