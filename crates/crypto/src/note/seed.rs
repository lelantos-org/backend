//! Per-output randomness expanded from the plaintext's `rseed`. Mirrors
//! `expandSeed` in `sdk/src/notes/seed.ts`:
//!
//! ```text
//! rcm   =     LE(blake2b-512("lelantos.note.rcm.v2"  || rseed || LE32(rho))) mod BN254_FR
//! esk   = 1 + LE(blake2b-512("lelantos.note.esk.v2"  || rseed || LE32(rho))) mod (q - 1)
//! fmd_r = 1 + LE(blake2b-512("lelantos.note.fmdr.v2" || rseed || LE32(rho))) mod (q - 1)
//! ```
//!
//! `q` is the Baby-Jubjub subgroup order. The sender publishes `epk = esk·g_d`
//! and the clue flagged with `fmd_r` on `g_d`; the wallet keeps a note only when
//! both are the ones its seed yields, so a verifier that credits a note must
//! check the same ([`ExpandedSeed::published`]).

use super::{AddressKeys, SEED_BYTES};
use crate::clue::{CircomPoint, expected_clue, scalar_mul};
use crate::tree::{Field, fq_to_be};
use ark_ed_on_bn254::{Fq, Fr};
use ark_ff::PrimeField;
use blake2::Blake2b512;
use blake2::digest::Digest;
use num_bigint::BigUint;

const RCM_DOMAIN: &[u8] = b"lelantos.note.rcm.v2";
const ESK_DOMAIN: &[u8] = b"lelantos.note.esk.v2";
const FMD_R_DOMAIN: &[u8] = b"lelantos.note.fmdr.v2";

/// The randomness of one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpandedSeed {
    /// Commitment blinder.
    pub rcm: Field,
    /// ECDH ephemeral scalar, in `[1, q - 1]`.
    pub esk: Fr,
    /// FMD clue blinder, in `[1, q - 1]`.
    pub fmd_r: Fr,
}

/// What a sender publishes beside an output's ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Published {
    pub epk: CircomPoint,
    pub clue_r: CircomPoint,
    /// Bit `i` is clue bit `i`: the wire prefix read big-endian.
    pub clue_bits: u16,
}

impl ExpandedSeed {
    /// What an honest sender publishes for this seed to the address `keys`
    /// belongs to.
    pub fn published(&self, keys: &AddressKeys) -> Published {
        let (clue_r, clue_bits) = expected_clue(&keys.detection_key, keys.g_d, self.fmd_r);
        Published {
            epk: scalar_mul(keys.g_d, self.esk),
            clue_r,
            clue_bits,
        }
    }
}

/// The randomness of the output whose plaintext carries `rseed` and whose note
/// has `rho`. `rho` must be canonical.
pub fn expand_seed(rseed: &[u8; SEED_BYTES], rho: &Field) -> ExpandedSeed {
    let mut rho_le = *rho;
    rho_le.reverse();
    let wide = |domain: &[u8]| {
        Blake2b512::new()
            .chain_update(domain)
            .chain_update(rseed)
            .chain_update(rho_le)
            .finalize()
    };
    let q_minus_1 = BigUint::from(Fr::MODULUS) - 1u8;
    let scalar = |domain: &[u8]| {
        Fr::from(BigUint::from_bytes_le(&wide(domain)) % &q_minus_1) + Fr::from(1u8)
    };
    ExpandedSeed {
        rcm: fq_to_be(Fq::from_le_bytes_mod_order(&wide(RCM_DOMAIN))),
        esk: scalar(ESK_DOMAIN),
        fmd_r: scalar(FMD_R_DOMAIN),
    }
}
