use ark_ed_on_bn254::Fq;
use ark_ff::{BigInteger, PrimeField};
use light_poseidon::{Poseidon, PoseidonHasher};

use super::{Field, TreeError};

pub(super) const ARITY: usize = 4;
pub const TAG_MERKLE: u64 = 5;

pub(super) fn fq_to_be(x: Fq) -> Field {
    let mut out = [0u8; 32];
    let bytes = x.into_bigint().to_bytes_be();
    let off = 32 - bytes.len();
    out[off..].copy_from_slice(&bytes);
    out
}

pub(super) fn be_to_fq(x: &Field) -> Fq {
    Fq::from_be_bytes_mod_order(x)
}

pub(super) fn hash_node(
    c0: &Field,
    c1: &Field,
    c2: &Field,
    c3: &Field,
) -> Result<Field, TreeError> {
    let mut p = Poseidon::<Fq>::new_circom(5).map_err(|e| TreeError::Poseidon(e.to_string()))?;
    let inputs = [
        Fq::from(TAG_MERKLE),
        be_to_fq(c0),
        be_to_fq(c1),
        be_to_fq(c2),
        be_to_fq(c3),
    ];
    let out = p
        .hash(&inputs)
        .map_err(|e| TreeError::Poseidon(e.to_string()))?;
    Ok(fq_to_be(out))
}
