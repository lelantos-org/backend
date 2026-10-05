//! Keys of one diversified address. Mirrors `sdk/src/keys/diversified.ts` and
//! `sdk/src/crypto/diversified-base.ts`.
//!
//! ```text
//! g_d     = diversified_base(d)
//! pk      = Poseidon(TAG_PK, ivk, d)
//! pk_d    = (ivk mod q)·g_d
//! dk_root = Poseidon(TAG_DK, ivk) mod q
//! ck_d    = dk_root·g_d
//! ```

use super::{TAG_DK, TAG_GD, derive_pk};
use crate::clue::{CircomPoint, detection_key, fq_to_scalar, scalar_mul, unpack};
use crate::poseidon::{self, PoseidonError};
use crate::tree::{Field, be_to_fq};
use ark_ed_on_bn254::{Fq, Fr};
use ark_ff::{BigInteger, PrimeField};

/// Counters tried for `g_d`. All failing has probability about `2^-256`.
const GD_COUNTERS: u64 = 256;

/// What the holder of `ivk` derives for the address with diversifier `d`.
///
/// No `Debug`: the detection scalars identify every note sent to `ivk`.
#[derive(Clone)]
pub struct AddressKeys {
    pub d: Field,
    pub g_d: CircomPoint,
    pub pk: Field,
    pub pk_d: CircomPoint,
    pub ck_d: CircomPoint,
    /// [`detection_key`] of `dk_root`. The same for every `d`.
    pub detection_key: Vec<Fr>,
}

/// The base point of the address with diversifier `d`: in the prime-order
/// subgroup, never the identity.
///
/// ```text
/// for ctr in 0..256:
///   P0 = unpack(LE32(Poseidon(TAG_GD, d, ctr)))   skip ctr if it does not decode
///   G  = [8]·P0
///   if G != identity: return G
/// ```
pub fn diversified_base(d: &Field) -> Result<CircomPoint, PoseidonError> {
    let d = be_to_fq(d);
    for ctr in 0..GD_COUNTERS {
        let y = poseidon::hash(&[Fq::from(TAG_GD), d, Fq::from(ctr)])?;
        let Ok(p0) = unpack(&y.into_bigint().to_bytes_le()) else {
            continue;
        };
        let g = scalar_mul(p0, Fr::from(8u64));
        if !g.is_identity() {
            return Ok(g);
        }
    }
    unreachable!("diversified base: no counter in [0, 256) yields a point")
}

/// Every key `ivk` holds for the address with diversifier `d`. An address is
/// `ivk`'s when its `pk`, `pk_d` and `ck_d` all equal these.
pub fn address_keys(ivk: &Field, d: &Field) -> Result<AddressKeys, PoseidonError> {
    let g_d = diversified_base(d)?;
    let dk_root = fq_to_scalar(poseidon::hash(&[Fq::from(TAG_DK), be_to_fq(ivk)])?);
    Ok(AddressKeys {
        d: *d,
        g_d,
        pk: derive_pk(ivk, d)?,
        pk_d: scalar_mul(g_d, Fr::from_be_bytes_mod_order(ivk)),
        ck_d: scalar_mul(g_d, dk_root),
        detection_key: detection_key(dk_root),
    })
}
