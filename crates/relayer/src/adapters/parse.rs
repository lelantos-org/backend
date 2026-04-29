use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use std::str::FromStr;

pub fn parse_u256(s: &str) -> AppResult<U256> {
    U256::from_str_radix(
        s.trim_start_matches("0x"),
        if s.starts_with("0x") { 16 } else { 10 },
    )
    .map_err(|e| AppError::BadRequest(format!("u256 parse: {}", e)))
}

pub fn parse_b32(s: &str) -> AppResult<FixedBytes<32>> {
    let v = parse_u256(s)?;
    Ok(FixedBytes::<32>::from(v.to_be_bytes::<32>()))
}

pub fn parse_address(s: &str) -> AppResult<Address> {
    Address::from_str(s).map_err(|e| AppError::BadRequest(format!("address parse: {}", e)))
}

/// Decode an optionally `0x`-prefixed hex string into raw bytes. `field` is
/// used only to build the error message.
pub fn parse_hex_bytes(s: &str, field: &'static str) -> AppResult<Bytes> {
    let raw = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| AppError::BadRequest(format!("{field} hex: {e}")))?;
    Ok(Bytes::from(raw))
}
