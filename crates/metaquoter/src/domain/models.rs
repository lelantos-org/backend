//! Wire types for `POST /v1/quotes`.
//!
//! Field names are snake_case on the wire, matching the Rust structs, and every
//! amount is a decimal string rather than a JSON number — see `u256_dec`.

use crate::domain::fees::BPS_DENOMINATOR;
use alloy::primitives::{Address, Bytes, U256};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Quote request from the SDK.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct QuoteRequest {
    /// EVM chain id (1 = mainnet, 8453 = Base, ...).
    pub chain_id: u64,
    /// Address of the ERC20 the user spends shielded (token A).
    #[schema(value_type = String, example = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")]
    pub token_in: Address,
    /// Address of the ERC20 the user receives shielded (token B).
    #[schema(value_type = String, example = "0x6B175474E89094C44Da98b954EedeAC495271d0F")]
    pub token_out: Address,
    /// Amount of `token_in` to swap, in the token's smallest unit. Encoded as a
    /// decimal string to avoid JSON precision loss above 2^53.
    #[serde(with = "u256_dec")]
    #[schema(value_type = String, example = "1000000000")]
    pub amount_in: U256,
    /// Caller's maximum slippage in basis points (50 = 0.5%).
    pub slippage_bps: u16,
}

impl QuoteRequest {
    /// Apply the request's slippage tolerance to a quoter's `expected_out`,
    /// producing the floor used as `min_out` on-chain:
    /// `expected_out * (10_000 - slippage_bps) / 10_000`, computed with
    /// saturating operations so adversarial inputs cannot panic.
    pub fn apply_slippage(&self, expected_out: U256) -> U256 {
        let factor = BPS_DENOMINATOR - self.slippage_bps.min(BPS_DENOMINATOR);
        expected_out.saturating_mul(U256::from(factor)) / U256::from(BPS_DENOMINATOR)
    }
}

/// Best route found across the racing quoters. Returned to the SDK as JSON.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Quote {
    pub venue: Venue,
    /// Allowlisted `ISwapAdapter` address bound to the route on-chain.
    #[schema(value_type = String, example = "0x0000000000000000000000000000000000000000")]
    pub adapter: Address,
    /// Adapter-specific opaque blob, 0x-prefixed hex, two 32-byte words for
    /// both venues implemented here: `abi.encode(uint24 fee, uint160
    /// sqrtPriceLimitX96)` for UniV3 and `abi.encode(uint24 fee, int24
    /// tickSpacing)` for UniV4. Only the bound `adapter` decodes it.
    #[schema(
        value_type = String,
        example = "0x00000000000000000000000000000000000000000000000000000000000001f4\
                   000000000000000000000000000000000000000000000000000000000000000a"
    )]
    pub route: Bytes,
    /// Quoter's expected output before slippage adjustment.
    #[serde(with = "u256_dec")]
    #[schema(value_type = String, example = "990000000")]
    pub expected_out: U256,
    /// `expected_out * (10_000 - slippage_bps) / 10_000`.
    #[serde(with = "u256_dec")]
    #[schema(value_type = String, example = "985050000")]
    pub min_out: U256,
    /// Wrapper-overhead-included gas estimate.
    pub gas_estimate: u64,
    /// Unix seconds at which the venue was queried, so a client can compute
    /// quote age.
    pub quoted_at: u64,
    /// MASP wrapper fee deducted from the venue's gross output, in the smallest
    /// unit of `token_out`. Already applied to `expected_out` and `min_out`.
    #[serde(with = "u256_dec")]
    #[schema(value_type = String, example = "0")]
    pub masp_fee: U256,
    /// MASP fee rate in basis points used to compute `masp_fee`.
    pub masp_fee_bps: u16,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Venue {
    UniV3,
    UniV4,
}

/// Wire format for [`U256`] is a decimal string, mirroring how the relayer SDK
/// encodes shielded amounts.
mod u256_dec {
    use alloy::primitives::U256;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(v: &U256, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
        let s = String::deserialize(d)?;
        U256::from_str_radix(&s, 10).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn req(slippage_bps: u16) -> QuoteRequest {
        QuoteRequest {
            chain_id: 1,
            token_in: address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
            token_out: address!("6b175474e89094c44da98b954eedeac495271d0f"),
            amount_in: U256::from(1_000_000u64),
            slippage_bps,
        }
    }

    #[test]
    fn slippage_math_basis_points() {
        let expected = U256::from(1_000_000u64);
        assert_eq!(req(50).apply_slippage(expected), U256::from(995_000u64));
        assert_eq!(req(0).apply_slippage(expected), expected);
        assert_eq!(req(10_000).apply_slippage(expected), U256::ZERO);
    }

    /// `slippage_bps` is a `u16` and the handler caps it well below this, but
    /// the clamp is what keeps the subtraction inside `apply_slippage` from
    /// underflowing if either ever changes.
    #[test]
    fn slippage_above_the_denominator_floors_at_zero() {
        assert_eq!(
            req(u16::MAX).apply_slippage(U256::from(1_000_000u64)),
            U256::ZERO
        );
    }

    /// Amounts above 2^53 are why the wire format is a decimal string; a JSON
    /// number would not survive the round trip.
    #[test]
    fn u256_amounts_round_trip_as_decimal_strings() {
        let raw = r#"{"chain_id":1,
                      "token_in":"0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                      "token_out":"0x6b175474e89094c44da98b954eedeac495271d0f",
                      "amount_in":"115792089237316195423570985008687907853269984665640564039457584007913129639935",
                      "slippage_bps":50}"#;
        let parsed: QuoteRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.amount_in, U256::MAX);
    }

    /// A JSON number is refused rather than silently truncated.
    #[test]
    fn a_json_number_amount_is_refused() {
        let raw = r#"{"chain_id":1,
                      "token_in":"0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                      "token_out":"0x6b175474e89094c44da98b954eedeac495271d0f",
                      "amount_in":1000000,
                      "slippage_bps":50}"#;
        assert!(serde_json::from_str::<QuoteRequest>(raw).is_err());
    }
}
