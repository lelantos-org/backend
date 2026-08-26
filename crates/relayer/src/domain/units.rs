//! Circuit units and ERC-20 base units.
//!
//! A note's `value` is a circuit unit range-checked to 64 bits, and the token
//! amount it stands for is `value * scale`, where `scale` is the per-asset
//! multiplier `AssetRegistry` publishes and `contracts/src/AssetRegistry.sol`
//! holds. Prices, gas costs and quotes are in base units.
//!
//! The two directions are asymmetric, which is why this is a type rather than
//! two loose helpers: multiplying up is exact, while dividing down must round,
//! and rounding the wrong way underpays.
//!
//! Mirrors `sdk/src/core/units.ts`, which enforces the same relation on the
//! wallet side.

use alloy::primitives::U256;
use bigdecimal::BigDecimal;
use num_bigint::{Sign, ToBigInt};

/// One asset's circuit-to-base multiplier. Always positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale(U256);

impl Scale {
    /// Read a scale off an `assets` row.
    ///
    /// `None` for anything that cannot be a multiplier: zero, negative or
    /// fractional. `NUMERIC` permits all three, and `to_bigint` would truncate a
    /// fractional value into a different multiplier, so this rejects rather than
    /// rounds.
    pub fn from_decimal(scale: &BigDecimal) -> Option<Self> {
        if !scale.is_integer() {
            return None;
        }
        let n = scale.to_bigint()?;
        if n.sign() != Sign::Plus {
            return None;
        }
        U256::from_str_radix(&n.to_string(), 10).ok().map(Self)
    }

    /// What `circuit_value` is worth in ERC-20 base units. Exact.
    ///
    /// Takes a `u128` so a sum of note values fits without a widening step at the
    /// call site; a single note's value is bounded to 64 bits by the circuit.
    /// Cannot overflow, since a scale wide enough to overflow `U256` could not
    /// have been registered on chain.
    pub fn to_base(self, circuit_value: u128) -> U256 {
        U256::from(circuit_value) * self.0
    }

    /// The smallest whole note value worth at least `base` base units.
    ///
    /// Rounds up. A note value is a whole circuit unit, so rounding down would
    /// underpay by as much as `scale - 1` and be refused.
    pub fn to_circuit_ceil(self, base: U256) -> U256 {
        (base + self.0 - U256::from(1u8)) / self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn scale(s: &str) -> Scale {
        Scale::from_decimal(&BigDecimal::from_str(s).expect("decimal")).expect("usable scale")
    }

    #[test]
    fn multiplies_up_exactly() {
        assert_eq!(
            scale("1000000000000").to_base(250),
            U256::from(250_000_000_000_000u64)
        );
        assert_eq!(scale("1").to_base(7), U256::from(7u8));
    }

    /// The direction that must round, rounding up.
    #[test]
    fn divides_down_by_rounding_up() {
        let s = scale("1000");
        assert_eq!(s.to_circuit_ceil(U256::from(0u8)), U256::ZERO);
        assert_eq!(s.to_circuit_ceil(U256::from(1u8)), U256::from(1u8));
        assert_eq!(s.to_circuit_ceil(U256::from(999u16)), U256::from(1u8));
        assert_eq!(s.to_circuit_ceil(U256::from(1000u16)), U256::from(1u8));
        assert_eq!(s.to_circuit_ceil(U256::from(1001u16)), U256::from(2u8));
    }

    /// Round-tripping a quote must never come back worth less than it started.
    #[test]
    fn a_rounded_up_note_always_covers_the_amount_it_came_from() {
        let s = scale("1000");
        for base in [1u128, 999, 1000, 1001, 123_456] {
            let note: u128 = s.to_circuit_ceil(U256::from(base)).to();
            assert!(s.to_base(note) >= U256::from(base), "base {base} underpaid");
        }
    }

    #[test]
    fn refuses_a_scale_that_is_not_a_positive_whole_multiplier() {
        for bad in ["0", "-1", "0.5", "1.0001"] {
            assert!(
                Scale::from_decimal(&BigDecimal::from_str(bad).expect("decimal")).is_none(),
                "{bad} must not be usable as a scale"
            );
        }
    }

    /// `1.0` is fractional in spelling only: `is_integer` accepts it and it is a
    /// valid multiplier.
    #[test]
    fn accepts_an_integer_written_with_a_decimal_point() {
        assert_eq!(scale("1.0").to_base(3), U256::from(3u8));
    }
}
