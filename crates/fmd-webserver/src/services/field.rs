//! Field-element conversions for the wire format.
//!
//! Coordinates are stored as `NUMERIC(78, 0)` and served as `0x`-prefixed hex.
//! The prefix is load-bearing: the SDK routes these through a decoder that
//! accepts decimal *or* `0x`-hex, so bare hex whose digits happen to all be
//! decimal would be silently parsed as the wrong number.

use crate::domain::error::{AppError, AppResult};
use fmd_crypto::tree::Field;

/// Big-endian 32-byte field element from a non-negative `NUMERIC` column.
pub fn bigdec_to_field(v: &bigdecimal::BigDecimal) -> AppResult<Field> {
    use bigdecimal::num_bigint::Sign;
    let (bi, _) = v.as_bigint_and_exponent();
    if bi.sign() == Sign::Minus {
        return Err(AppError::Internal(format!("negative field element: {v}")));
    }
    let bytes = bi.to_bytes_be().1;
    if bytes.len() > 32 {
        return Err(AppError::Internal(format!(
            "field element > 32 bytes: {} bytes",
            bytes.len()
        )));
    }
    let mut f = [0u8; 32];
    f[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(f)
}

/// Big-endian 32-byte field element from a `BYTEA` column, left-padded.
///
/// Postgres does not pad `BYTEA`, so a commitment with leading zero bytes
/// comes back short. Widening on the left preserves the value; taking the
/// bytes as-is would shift it.
pub fn bytes_to_field(v: &[u8]) -> AppResult<Field> {
    if v.len() > 32 {
        return Err(AppError::Internal(format!(
            "field element > 32 bytes: {} bytes",
            v.len()
        )));
    }
    let mut f = [0u8; 32];
    f[32 - v.len()..].copy_from_slice(v);
    Ok(f)
}

/// Fixed-width `0x` + 64 hex chars, zero-padded on the left.
pub fn field_to_hex(f: &Field) -> String {
    format!("0x{}", hex::encode(f))
}

/// `NUMERIC` column straight to the wire form.
pub fn bigdec_to_hex(v: &bigdecimal::BigDecimal) -> AppResult<String> {
    Ok(field_to_hex(&bigdec_to_field(v)?))
}

/// The Baby-Jubjub base field modulus, `q`, in decimal.
///
/// Only [`Q_HALF_BE`] derives from it, and only in a test — it exists so that
/// constant is checkable against something rather than trusted as transcribed.
#[cfg(test)]
const Q_DEC: &str = "21888242871839275222246405745257275088548364400416034343698204186575808495617";

/// Half the Baby-Jubjub base field modulus, `(q-1)/2`, big-endian.
///
/// circomlibjs calls `x` negative when `x > (q-1)/2`; that predicate is the
/// packed point's sign bit. Comparing fixed-width big-endian arrays is the
/// same as comparing the numbers, so this is a plain `>` below.
///
/// Pinned by `q_half_matches_the_modulus` — a mis-transcribed digit here
/// flips the sign bit for a band of `x` values and nothing else would notice.
const Q_HALF_BE: Field = [
    0x18, 0x32, 0x27, 0x39, 0x70, 0x98, 0xd0, 0x14, 0xdc, 0x28, 0x22, 0xdb, 0x40, 0xc0, 0xac, 0x2e,
    0x94, 0x19, 0xf4, 0x24, 0x3c, 0xdc, 0xb8, 0x48, 0xa1, 0xf0, 0xfa, 0xc9, 0xf8, 0x00, 0x00, 0x00,
];

/// Pack a Baby-Jubjub point into the 32 bytes circomlibjs `babyJub.packPoint`
/// produces: `y` little-endian, with the high bit of the last byte set when
/// `x > (q-1)/2`.
///
/// `x` is recoverable from `y` and that one bit, so only `y` travels — halving
/// what an ephemeral public key costs on a feed every wallet downloads in full.
///
/// **Little-endian, unlike every other field here.** This is not a number the
/// client parses, it is the exact byte string `decryptNote` expects as `epk`,
/// so it mirrors `sdk/wasm/jubjub/src/curve.rs::compress` byte for byte rather
/// than this module's big-endian convention. Serving it big-endian would
/// decode to a different, valid-looking point.
pub fn pack_point(x: &bigdecimal::BigDecimal, y: &bigdecimal::BigDecimal) -> AppResult<[u8; 32]> {
    let x_be = coordinate(x, "x")?;
    // `bigdec_to_field` yields big-endian; the packed form is little-endian.
    let mut packed = coordinate(y, "y")?;
    packed.reverse();
    if x_be > Q_HALF_BE {
        packed[31] |= 0x80;
    }
    Ok(packed)
}

/// [`bigdec_to_field`] naming the coordinate it rejected.
///
/// Both coordinates fail the same way, so without this a bad row reports only
/// that *some* field element was malformed — on a feed of a million notes that
/// is the difference between a one-line fix and a hunt.
fn coordinate(v: &bigdecimal::BigDecimal, name: &str) -> AppResult<Field> {
    bigdec_to_field(v).map_err(|e| AppError::Internal(format!("ephemeral pubkey {name}: {e}")))
}

/// [`pack_point`] straight to the wire form.
///
/// Bare hex, not `0x`-prefixed: the prefix disambiguates *numbers* for the
/// SDK's decimal-or-hex decoder, and this is a byte string routed through the
/// same path as `ciphertextHex`.
pub fn pack_point_hex(x: &bigdecimal::BigDecimal, y: &bigdecimal::BigDecimal) -> AppResult<String> {
    Ok(hex::encode(pack_point(x, y)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> bigdecimal::BigDecimal {
        bigdecimal::BigDecimal::from_str(s).unwrap()
    }

    /// circomlibjs `Base8`, the generator every SDK point derives from.
    fn base8() -> (bigdecimal::BigDecimal, bigdecimal::BigDecimal) {
        (
            dec("5299619240641551281634865583518297030282874472190772894086521144482721001553"),
            dec("16950150798460657717958625567821834550301663161624707787222815936182638968203"),
        )
    }

    #[test]
    fn packs_a_point_as_little_endian_y() {
        // Byte-for-byte `babyJub.packPoint(Base8)`. Base8.x < (q-1)/2, so the
        // sign bit stays clear and this is plain little-endian `y`.
        let (x, y) = base8();
        assert_eq!(
            pack_point_hex(&x, &y).unwrap(),
            "8b7d2d877a253c4b7733e1b91f05e0fcedf96bd11c2e572549b2a0f703727925"
        );
    }

    #[test]
    fn q_half_matches_the_modulus() {
        // Derives `(q-1)/2` rather than trusting the transcription. The
        // hand-copied version of this constant was wrong, and only a
        // known-answer test caught it — indirectly, and with a confusing
        // message. This one names the actual fault.
        let want = bigdec_to_field(&((dec(Q_DEC) - dec("1")) / dec("2"))).unwrap();
        assert_eq!(Q_HALF_BE, want);
    }

    #[test]
    fn sets_the_sign_bit_for_a_negative_x() {
        // Negating x flips only the sign bit: 0x25 -> 0xa5. Getting this
        // inverted yields a valid-looking point on the wrong branch, which no
        // length or format check would catch.
        let (x, y) = base8();
        let packed = pack_point_hex(&(dec(Q_DEC) - x), &y).unwrap();
        assert_eq!(
            packed,
            "8b7d2d877a253c4b7733e1b91f05e0fcedf96bd11c2e572549b2a0f7037279a5"
        );
    }

    #[test]
    fn packs_the_two_x_parities_to_the_same_y_bytes() {
        let (x, y) = base8();
        let pos = pack_point(&x, &y).unwrap();
        let neg = pack_point(&(dec(Q_DEC) - x), &y).unwrap();
        assert_eq!(pos[..31], neg[..31], "only the last byte may differ");
        assert_eq!(pos[31] | 0x80, neg[31]);
    }

    #[test]
    fn treats_exactly_half_the_modulus_as_non_negative() {
        // circomlibjs is `x > (q-1)/2`, strictly. An `>=` here would flip the
        // sign bit for one specific x and corrupt only those notes.
        let q_half =
            dec("10944121435919637611123202872628637544274182200208017171849102093287904247808");
        let (_, y) = base8();
        assert_eq!(pack_point(&q_half, &y).unwrap()[31] & 0x80, 0);
        assert_eq!(
            pack_point(&(q_half + dec("1")), &y).unwrap()[31] & 0x80,
            0x80
        );
    }

    #[test]
    fn names_the_coordinate_it_rejects() {
        let (x, y) = base8();
        let on_x = pack_point(&dec("-1"), &y).unwrap_err().to_string();
        let on_y = pack_point(&x, &dec("-1")).unwrap_err().to_string();
        assert!(on_x.contains("ephemeral pubkey x"), "{on_x}");
        assert!(on_y.contains("ephemeral pubkey y"), "{on_y}");
    }

    #[test]
    fn encodes_fixed_width_with_leading_zeros() {
        // Small values must stay left-padded — a variable-width hex string
        // would change the byte length the client reads back.
        assert_eq!(
            bigdec_to_hex(&dec("1")).unwrap(),
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        );
        assert_eq!(
            bigdec_to_hex(&dec("0")).unwrap(),
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn encodes_the_full_width_bn254_modulus_minus_one() {
        // Largest value a 254-bit Fq can hold: must fit in exactly 64 hex chars.
        let p_minus_1 =
            dec("21888242871839275222246405745257275088548364400416034343698204186575808495616");
        let hex = bigdec_to_hex(&p_minus_1).unwrap();
        assert_eq!(hex.len(), 66, "0x + 64 chars");
        assert_eq!(
            hex,
            "0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000000"
        );
    }

    #[test]
    fn round_trips_through_the_field_representation() {
        let v = dec("12345678901234567890123456789012345678901234567890");
        let f = bigdec_to_field(&v).unwrap();
        let back = bigdecimal::BigDecimal::from(bigdecimal::num_bigint::BigInt::from_bytes_be(
            bigdecimal::num_bigint::Sign::Plus,
            &f,
        ));
        assert_eq!(back, v);
    }

    #[test]
    fn left_pads_a_short_bytea() {
        // Postgres returns BYTEA unpadded, so a commitment with leading zeros
        // arrives short. Padding on the right would multiply it by 2^8n.
        assert_eq!(bytes_to_field(&[1, 2]).unwrap()[30..], [1, 2]);
        assert_eq!(bytes_to_field(&[1, 2]).unwrap()[..30], [0u8; 30]);
        assert_eq!(bytes_to_field(&[]).unwrap(), [0u8; 32]);
        assert_eq!(bytes_to_field(&[7u8; 32]).unwrap(), [7u8; 32]);
    }

    #[test]
    fn rejects_a_bytea_wider_than_a_field_element() {
        assert!(bytes_to_field(&[0u8; 33]).is_err());
    }

    #[test]
    fn rejects_values_that_cannot_be_a_field_element() {
        assert!(bigdec_to_field(&dec("-1")).is_err(), "negative");
        // 33 bytes.
        let too_big = dec("1") * dec("256").powi(33);
        assert!(bigdec_to_field(&too_big).is_err(), "over 32 bytes");
    }
}
