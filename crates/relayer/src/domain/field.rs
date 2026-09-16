//! The scalar field every in-circuit value lives in.

use alloy::primitives::U256;
use std::sync::LazyLock;

/// BN254 scalar field order: the modulus every in-circuit signal lives under, and
/// the one `crypto::poseidon` enforces over `ark_ed_on_bn254::Fq`.
pub static BN254_R: LazyLock<U256> = LazyLock::new(|| {
    U256::from_str_radix(
        "21888242871839275222246405745257275088548364400416034343698204186575808495617",
        10,
    )
    .expect("BN254 modulus literal")
});
