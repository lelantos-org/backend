//! Postgres `NUMERIC` → `u64`, alongside `asset_registry::bigdecimal_to_u256`,
//! which the `asset-registry` crate owns because it prices a row with it.
//!
//! Kept apart from `adapters::parse`, which decodes wire input. These columns
//! were written by the indexer, so a bad value is an internal fault and maps to
//! [`AppError::Internal`] rather than `BadRequest`.

use crate::domain::error::{AppError, AppResult};
use ::asset_registry::bigdecimal_to_u256;
use bigdecimal::BigDecimal;

/// Same source columns as [`bigdecimal_to_u256`], narrowed to `u64`.
pub fn bigdecimal_to_u64(v: &BigDecimal) -> AppResult<u64> {
    bigdecimal_to_u256(v)?
        .try_into()
        .map_err(|_| AppError::Internal(format!("numeric out of u64 range: {v}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn u64_narrows_and_rejects_overflow() {
        let ok = BigDecimal::from_str("18446744073709551615").unwrap();
        assert_eq!(bigdecimal_to_u64(&ok).unwrap(), u64::MAX);

        let too_big = BigDecimal::from_str("18446744073709551616").unwrap();
        assert!(bigdecimal_to_u64(&too_big).is_err());
    }
}
