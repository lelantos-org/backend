//! Postgres `NUMERIC` → integer conversions.
//!
//! Kept apart from `adapters::parse`, which decodes wire input. These columns
//! were written by the indexer, so a bad value is an internal fault and maps to
//! [`AppError::Internal`] rather than `BadRequest`.

use crate::domain::error::{AppError, AppResult};
use alloy::primitives::U256;
use bigdecimal::BigDecimal;
use bigdecimal::num_bigint::{Sign, ToBigInt};

/// Non-negative integer `NUMERIC` → `U256`. Covers the BN254 coordinates,
/// BJJ scalars, and deposit amounts this crate reads.
///
/// Goes through `BigInt` rather than `to_string`, and tests `is_integer` rather
/// than `as_bigint_and_exponent().1 == 0`. An integer may arrive with a non-zero
/// scale, where `Display` switches to scientific notation (`1E+20`) that a
/// radix-10 parse rejects, and the exponent test would reject the value outright.
pub fn bigdecimal_to_u256(v: &BigDecimal) -> AppResult<U256> {
    if !v.is_integer() {
        return Err(AppError::Internal(format!(
            "numeric has fractional part: {v}"
        )));
    }
    let bi = v
        .to_bigint()
        .ok_or_else(|| AppError::Internal(format!("numeric not representable: {v}")))?;
    if bi.sign() == Sign::Minus {
        return Err(AppError::Internal(format!("numeric is negative: {v}")));
    }
    let bytes = bi.to_bytes_be().1;
    if bytes.len() > 32 {
        return Err(AppError::Internal(format!("numeric exceeds 32 bytes: {v}")));
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(U256::from_be_bytes(buf))
}

/// Same source columns as [`bigdecimal_to_u256`], narrowed to `u64`.
pub fn bigdecimal_to_u64(v: &BigDecimal) -> AppResult<u64> {
    bigdecimal_to_u256(v)?
        .try_into()
        .map_err(|_| AppError::Internal(format!("numeric out of u64 range: {v}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::num_bigint::BigInt;
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

    #[test]
    fn u64_narrows_and_rejects_overflow() {
        let ok = BigDecimal::from_str("18446744073709551615").unwrap();
        assert_eq!(bigdecimal_to_u64(&ok).unwrap(), u64::MAX);

        let too_big = BigDecimal::from_str("18446744073709551616").unwrap();
        assert!(bigdecimal_to_u64(&too_big).is_err());
    }
}
