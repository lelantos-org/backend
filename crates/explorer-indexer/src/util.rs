use alloy::primitives::U256;
use bigdecimal::{BigDecimal, num_bigint::BigInt, num_bigint::Sign};

pub fn u256_to_bigdecimal(v: U256) -> BigDecimal {
    let bytes = v.to_be_bytes::<32>();
    BigDecimal::from(BigInt::from_bytes_be(Sign::Plus, &bytes))
}
