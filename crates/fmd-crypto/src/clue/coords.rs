//! Baby-Jubjub coordinate ops (circomlib frame).
//!
//! Coordinate frame: circomlib BabyJubJub (a=168700, d=168696). `ark_ed_on_bn254`
//! uses an isomorphic a=1 form, so `x_ark = x_circom · √168700` bridges into ark
//! for scalar multiplication and results are converted back. Compression and bit
//! hashing happen in circomlib coordinates.
//!
//! Compression follows circomlibjs: y as 32-byte little-endian, with the sign of
//! x in the top bit of byte 31, where sign is `x > (p-1)/2` as an integer.

use ark_ec::CurveGroup;
use ark_ec::scalar_mul::fixed_base::FixedBase;
use ark_ed_on_bn254::{EdwardsAffine, EdwardsProjective, Fq, Fr};
use ark_ff::{BigInteger, Field, PrimeField, Zero};
use std::str::FromStr;
use std::sync::OnceLock;

use super::ClueError;

pub const COEFF_A_CIRCOM: u64 = 168700;
pub const COEFF_D_CIRCOM: u64 = 168696;

fn s_factor() -> Fq {
    static S: OnceLock<Fq> = OnceLock::new();
    *S.get_or_init(|| Fq::from(COEFF_A_CIRCOM).sqrt().expect("sqrt(168700) ∈ Fq"))
}

fn s_factor_inv() -> Fq {
    static SI: OnceLock<Fq> = OnceLock::new();
    *SI.get_or_init(|| s_factor().inverse().expect("nonzero"))
}

fn x_is_negative(x: &Fq) -> bool {
    let half = <Fq as PrimeField>::MODULUS_MINUS_ONE_DIV_TWO;
    x.into_bigint() > half
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CircomPoint {
    pub x: Fq,
    pub y: Fq,
}

impl CircomPoint {
    pub fn new(x: Fq, y: Fq) -> Self {
        Self { x, y }
    }

    pub fn is_on_curve(&self) -> bool {
        let a = Fq::from(COEFF_A_CIRCOM);
        let d = Fq::from(COEFF_D_CIRCOM);
        let x2 = self.x.square();
        let y2 = self.y.square();
        a * x2 + y2 == Fq::ONE + d * x2 * y2
    }

    fn to_ark(self) -> EdwardsAffine {
        EdwardsAffine::new_unchecked(self.x * s_factor(), self.y)
    }

    fn from_ark(p: EdwardsAffine) -> Self {
        Self {
            x: p.x * s_factor_inv(),
            y: p.y,
        }
    }

    /// The twisted-Edwards identity `(0, 1)`.
    pub fn is_identity(&self) -> bool {
        self.x.is_zero() && self.y == Fq::ONE
    }

    /// Full order-`n` subgroup test, delegated to ark.
    ///
    /// Baby-Jubjub's group is `Z_8 x Z_n`, so a point on the curve is not
    /// necessarily in the prime-order subgroup. Callers that multiply by a secret
    /// require this check: an 8-torsion component reduces the product to one of
    /// eight values, leaking the secret. The weaker `[8]P == O` test is not
    /// sufficient, since it accepts `T + [t]B`.
    pub fn is_in_prime_subgroup(&self) -> bool {
        self.to_ark().is_in_correct_subgroup_assuming_on_curve()
    }
}

pub fn base8_circom() -> CircomPoint {
    let x = Fq::from_str(
        "5299619240641551281634865583518297030282874472190772894086521144482721001553",
    )
    .unwrap();
    let y = Fq::from_str(
        "16950150798460657717958625567821834550301663161624707787222815936182638968203",
    )
    .unwrap();
    CircomPoint::new(x, y)
}

pub fn scalar_mul(p: CircomPoint, k: Fr) -> CircomPoint {
    let proj: EdwardsProjective = p.to_ark().into();
    CircomPoint::from_ark((proj * k).into_affine())
}

/// Precomputed window table for repeated scalar multiplications against a fixed
/// base `p`. Building the table is amortized across many scalars: once built, `n`
/// scalar multiplications cost roughly `n * 32` additions instead of `n * 256`
/// doublings.
pub struct FixedBaseTable {
    table: Vec<Vec<EdwardsAffine>>,
    window: usize,
    scalar_size: usize,
}

impl FixedBaseTable {
    pub fn new(p: CircomPoint, hint_n: usize) -> Self {
        let scalar_size = <Fr as PrimeField>::MODULUS_BIT_SIZE as usize;
        let window = FixedBase::get_mul_window_size(hint_n.max(2));
        let proj: EdwardsProjective = p.to_ark().into();
        let table = FixedBase::get_window_table::<EdwardsProjective>(scalar_size, window, proj);
        Self {
            table,
            window,
            scalar_size,
        }
    }

    /// Compute `p * k` for each scalar in `scalars`, returning circomlib
    /// coordinates aligned with the input order.
    pub fn batch_mul(&self, scalars: &[Fr]) -> Vec<CircomPoint> {
        if scalars.is_empty() {
            return Vec::new();
        }
        let prods = FixedBase::msm::<EdwardsProjective>(
            self.scalar_size,
            self.window,
            &self.table,
            scalars,
        );
        let aff = EdwardsProjective::normalize_batch(&prods);
        aff.into_iter().map(CircomPoint::from_ark).collect()
    }
}

pub fn unpack(bytes: &[u8]) -> Result<CircomPoint, ClueError> {
    if bytes.len() != 32 {
        return Err(ClueError::BadLength);
    }
    let mut y_bytes = [0u8; 32];
    y_bytes.copy_from_slice(bytes);
    let sign_bit = (y_bytes[31] >> 7) & 1 == 1;
    y_bytes[31] &= 0x7f;
    let y = Fq::from_le_bytes_mod_order(&y_bytes);
    let a = Fq::from(COEFF_A_CIRCOM);
    let d = Fq::from(COEFF_D_CIRCOM);
    let y2 = y.square();
    let num = y2 - Fq::ONE;
    let den = d * y2 - a;
    let den_inv = den.inverse().ok_or(ClueError::NotOnCurve)?;
    let x2 = num * den_inv;
    let x = x2.sqrt().ok_or(ClueError::NotOnCurve)?;
    let x_final = if x_is_negative(&x) == sign_bit { x } else { -x };
    let p = CircomPoint::new(x_final, y);
    if !p.is_on_curve() {
        return Err(ClueError::NotOnCurve);
    }
    Ok(p)
}

/// [`unpack`], plus the two checks required of a point that will be multiplied by
/// a secret: prime-order subgroup membership and non-identity.
///
/// The identity absorbs any scalar, so a shared secret derived from it is the
/// same for every key. `unpack` accepts it because it satisfies the curve
/// equation, so this is a separate entry point.
pub fn unpack_subgroup(bytes: &[u8]) -> Result<CircomPoint, ClueError> {
    let p = unpack(bytes)?;
    if p.is_identity() || !p.is_in_prime_subgroup() {
        return Err(ClueError::NotOnCurve);
    }
    Ok(p)
}

pub fn pack(p: &CircomPoint) -> [u8; 32] {
    let y_bytes = p.y.into_bigint().to_bytes_le();
    let mut out = [0u8; 32];
    out[..y_bytes.len().min(32)].copy_from_slice(&y_bytes[..y_bytes.len().min(32)]);
    if x_is_negative(&p.x) {
        out[31] |= 0x80;
    }
    out
}
