use ark_ed_on_bn254::Fq;
use light_poseidon::{Poseidon, PoseidonBytesHasher, PoseidonHasher};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PoseidonError {
    #[error("light-poseidon: {0}")]
    Inner(String),
}

pub fn hash(inputs: &[Fq]) -> Result<Fq, PoseidonError> {
    let mut h = Poseidon::<Fq>::new_circom(inputs.len())
        .map_err(|e| PoseidonError::Inner(e.to_string()))?;
    h.hash(inputs)
        .map_err(|e| PoseidonError::Inner(e.to_string()))
}

pub fn hash_bytes_be(inputs: &[&[u8]]) -> Result<[u8; 32], PoseidonError> {
    let mut h = Poseidon::<Fq>::new_circom(inputs.len())
        .map_err(|e| PoseidonError::Inner(e.to_string()))?;
    h.hash_bytes_be(inputs)
        .map_err(|e| PoseidonError::Inner(e.to_string()))
}
