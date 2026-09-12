//! Conversions between an EVM word and the Postgres `NUMERIC` columns it is
//! stored in.
//!
//! Both directions live here because they are inverses: the indexers widen a
//! `U256` on the way in and the readers narrow it on the way out, and a change
//! to one that is not mirrored in the other is a silent corruption. Neither
//! touches the database — they convert the value, not the row.
//!
//! Errors are plain strings so each caller maps them into its own error enum;
//! this crate has no view of what a bad value means to the service reading it.

use alloy::primitives::U256;
use bigdecimal::BigDecimal;
use bigdecimal::num_bigint::{BigInt, Sign, ToBigInt};

/// `U256` → `NUMERIC`. Infallible: every `U256` is a non-negative integer, and
/// `NUMERIC` is unbounded.
pub fn u256_to_bigdecimal(v: U256) -> BigDecimal {
    let bytes = v.to_be_bytes::<32>();
    BigDecimal::from(BigInt::from_bytes_be(Sign::Plus, &bytes))
}

/// Non-negative integer `NUMERIC` → `U256`. Covers asset scales, venue balances
/// and the note commitment coordinates.
///
/// Goes through `BigInt` rather than `to_string`, and tests `is_integer` rather
/// than `as_bigint_and_exponent().1 == 0`. An integer may arrive with a non-zero
/// scale, where `Display` switches to scientific notation (`1E+20`) that a
/// radix-10 parse rejects, and the exponent test would reject the value outright.
pub fn bigdecimal_to_u256(v: &BigDecimal) -> Result<U256, String> {
    if !v.is_integer() {
        return Err(format!("numeric has fractional part: {v}"));
    }
    let bi = v
        .to_bigint()
        .ok_or_else(|| format!("numeric not representable: {v}"))?;
    if bi.sign() == Sign::Minus {
        return Err(format!("numeric is negative: {v}"));
    }
    let bytes = bi.to_bytes_be().1;
    if bytes.len() > 32 {
        return Err(format!("numeric exceeds 32 bytes: {v}"));
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(U256::from_be_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn u256(dec: &str) -> U256 {
        U256::from_str_radix(dec, 10).unwrap()
    }

    #[test]
    fn plain_integer_round_trips() {
        let v = BigDecimal::from_str("12345").unwrap();
        assert_eq!(bigdecimal_to_u256(&v).unwrap(), u256("12345"));
    }

    #[test]
    fn zero_round_trips() {
        assert_eq!(
            bigdecimal_to_u256(&BigDecimal::from(0u8)).unwrap(),
            U256::ZERO
        );
    }

    /// `BigDecimal::new(_, -20)` is the integer 10^20 held with a negative scale,
    /// which `Display` renders as `1E+20`. Neither an exponent check nor a
    /// `to_string` parse accepts this shape.
    #[test]
    fn integer_with_negative_scale_is_accepted() {
        let v = BigDecimal::new(BigInt::from(1), -20);
        assert!(
            !v.to_string().contains("00000"),
            "expected {v} to be scientific notation"
        );
        assert_eq!(
            bigdecimal_to_u256(&v).unwrap(),
            u256("100000000000000000000")
        );
    }

    #[test]
    fn trailing_zero_integer_is_accepted() {
        let v = BigDecimal::from_str("1000000000000000000").unwrap();
        assert_eq!(bigdecimal_to_u256(&v).unwrap(), u256("1000000000000000000"));
    }

    /// A BN254 coordinate is well under this; the 32-byte ceiling is what `U256`
    /// can hold.
    #[test]
    fn max_u256_is_accepted() {
        let v = BigDecimal::from_str(&U256::MAX.to_string()).unwrap();
        assert_eq!(bigdecimal_to_u256(&v).unwrap(), U256::MAX);
    }

    #[test]
    fn value_wider_than_32_bytes_is_rejected() {
        let v = BigDecimal::from_str(&(U256::MAX.to_string() + "0")).unwrap();
        assert!(bigdecimal_to_u256(&v).is_err());
    }

    #[test]
    fn fractional_is_rejected() {
        let v = BigDecimal::from_str("1.5").unwrap();
        assert!(bigdecimal_to_u256(&v).is_err());
    }

    #[test]
    fn negative_is_rejected() {
        let v = BigDecimal::from_str("-1").unwrap();
        assert!(bigdecimal_to_u256(&v).is_err());
    }

    /// The two directions are inverses. This is the property that keeps a change
    /// to one from silently diverging from the other.
    #[test]
    fn the_two_directions_round_trip() {
        for v in [
            U256::ZERO,
            U256::from(1u8),
            U256::from(10u64.pow(18)),
            U256::MAX,
        ] {
            assert_eq!(bigdecimal_to_u256(&u256_to_bigdecimal(v)).unwrap(), v);
        }
    }
}
