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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> bigdecimal::BigDecimal {
        bigdecimal::BigDecimal::from_str(s).unwrap()
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
