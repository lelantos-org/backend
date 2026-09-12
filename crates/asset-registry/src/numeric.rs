//! Postgres `NUMERIC` → integer conversions.
//!
//! The conversion itself lives in `chain_types::numeric`, beside its inverse.
//! This wrapper exists to map its error: these columns were written by the
//! indexer, so a bad value is an internal fault and maps to [`Error::Numeric`]
//! rather than anything a caller could have sent.

use crate::error::{Error, Result};
use alloy::primitives::U256;
use bigdecimal::BigDecimal;

/// Non-negative integer `NUMERIC` → `U256`. Covers the asset scales and venue
/// balances this crate reads.
pub fn bigdecimal_to_u256(v: &BigDecimal) -> Result<U256> {
    chain_types::numeric::bigdecimal_to_u256(v).map_err(Error::Numeric)
}
