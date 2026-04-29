use crate::domain::error::{AppError, AppResult};
use ark_ed_on_bn254::Fq;
use ark_ff::{BigInteger, PrimeField};
use fmd_crypto::tree::{Field, TAG_MERKLE};
use light_poseidon::{Poseidon, PoseidonHasher};

/// Domain-separation tag for Merkle leaf hashing — mirrors `TAG_LEAF` in
/// `circuits/src/lib/tags.circom`. `leaf = Poseidon(TAG_LEAF, cm, cv_dep_x, cv_dep_y)`.
pub const TAG_LEAF: u64 = 10;

/// Compute the in-circuit Merkle leaf hash from (cm, cv_dep_x, cv_dep_y).
pub fn leaf_hash(cm: &Field, cv_dep_x: &Field, cv_dep_y: &Field) -> AppResult<Field> {
    let mut p = Poseidon::<Fq>::new_circom(4).map_err(|e| AppError::Internal(e.to_string()))?;
    let inputs = [
        Fq::from(TAG_LEAF),
        Fq::from_be_bytes_mod_order(cm),
        Fq::from_be_bytes_mod_order(cv_dep_x),
        Fq::from_be_bytes_mod_order(cv_dep_y),
    ];
    let h = p
        .hash(&inputs)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let bytes = h.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(out)
}

/// Recompute root from a leaf + sibling path using the Circom-compatible
/// Poseidon-5 over BN254 with `TAG_MERKLE` domain separation.
pub fn recompute_root(
    leaf: &Field,
    path_elements: &[[Field; 3]],
    path_indices: &[u8],
) -> AppResult<Field> {
    let mut cur = *leaf;
    for (lvl, sibs) in path_elements.iter().enumerate() {
        let slot = path_indices[lvl] as usize;
        let mut children = [[0u8; 32]; 4];
        let mut s = 0;
        #[allow(clippy::needless_range_loop)]
        for k in 0..4 {
            if k == slot {
                children[k] = cur;
            } else {
                children[k] = sibs[s];
                s += 1;
            }
        }
        let mut p = Poseidon::<Fq>::new_circom(5).map_err(|e| AppError::Internal(e.to_string()))?;
        let inputs = [
            Fq::from(TAG_MERKLE),
            Fq::from_be_bytes_mod_order(&children[0]),
            Fq::from_be_bytes_mod_order(&children[1]),
            Fq::from_be_bytes_mod_order(&children[2]),
            Fq::from_be_bytes_mod_order(&children[3]),
        ];
        let h = p
            .hash(&inputs)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let bytes = h.into_bigint().to_bytes_be();
        let mut out = [0u8; 32];
        out[32 - bytes.len()..].copy_from_slice(&bytes);
        cur = out;
    }
    Ok(cur)
}
