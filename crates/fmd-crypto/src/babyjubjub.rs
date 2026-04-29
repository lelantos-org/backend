use ark_ec::{AffineRepr, CurveGroup};
use ark_ed_on_bn254::{EdwardsAffine, EdwardsProjective, Fr};
use ark_ff::PrimeField;

pub fn scalar_mul(point: &EdwardsAffine, scalar: &Fr) -> EdwardsAffine {
    (EdwardsProjective::from(*point) * scalar).into_affine()
}

pub fn pubkey_from_sk(sk: &Fr) -> EdwardsAffine {
    let g = EdwardsAffine::generator();
    scalar_mul(&g, sk)
}

pub fn fr_from_bytes_be(bytes: &[u8]) -> Fr {
    Fr::from_be_bytes_mod_order(bytes)
}
