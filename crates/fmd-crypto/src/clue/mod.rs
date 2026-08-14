//! FMD per `lelantos.fmd.v4` (poseidon + Legendre symbol scheme).

mod coords;
mod detect;

use ark_ed_on_bn254::{Fq, Fr};
use ark_ff::PrimeField;
use std::str::FromStr;
use thiserror::Error;

pub use coords::{
    COEFF_A_CIRCOM, COEFF_D_CIRCOM, CircomPoint, FixedBaseTable, base8_circom, pack, scalar_mul,
    unpack,
};
pub use detect::{TAG_FMD_BIT, test_clue, test_clue_batch};

pub const DOMAIN: &str = "lelantos.fmd.v4";

#[derive(Debug, Error)]
pub enum ClueError {
    #[error("invalid encoding length")]
    BadLength,
    #[error("invalid point: not on curve")]
    NotOnCurve,
    #[error("decode: {0}")]
    Decode(String),
}

pub fn point_from_xy(x: Fq, y: Fq) -> CircomPoint {
    CircomPoint::new(x, y)
}

pub fn fr_from_dec(s: &str) -> Fr {
    Fr::from_str(s).map_err(|_| "fr").unwrap()
}

pub fn fq_from_dec(s: &str) -> Fq {
    Fq::from_str(s).map_err(|_| "fq").unwrap()
}

pub fn fq_from_be_bytes(bytes: &[u8]) -> Fq {
    Fq::from_be_bytes_mod_order(bytes)
}

pub fn fq_from_le_bytes(bytes: &[u8]) -> Fq {
    Fq::from_le_bytes_mod_order(bytes)
}
