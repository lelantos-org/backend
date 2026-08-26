use crate::domain::error::{AppError, AppResult};
use ark_ed_on_bn254::Fq;
use ark_ff::{BigInteger, PrimeField};
use fmd_crypto::poseidon;
use fmd_crypto::tree::Field;

/// Domain-separation tag for Merkle leaf hashing, mirroring `TAG_LEAF` in
/// `circuits/src/lib/tags.circom`:
/// `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
pub const TAG_LEAF: u64 = 10;

/// Compute the in-circuit Merkle leaf hash from `(cm, cv_dep_x, cv_dep_y)`.
pub fn leaf_hash(cm: &Field, cv_dep_x: &Field, cv_dep_y: &Field) -> AppResult<Field> {
    let inputs = [
        Fq::from(TAG_LEAF),
        Fq::from_be_bytes_mod_order(cm),
        Fq::from_be_bytes_mod_order(cv_dep_x),
        Fq::from_be_bytes_mod_order(cv_dep_y),
    ];
    let h = poseidon::hash(&inputs).map_err(|e| AppError::Internal(e.to_string()))?;
    let bytes = h.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(out)
}
