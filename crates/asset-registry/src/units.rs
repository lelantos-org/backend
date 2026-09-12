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

/// One asset's circuit-to-base rate.
///
/// A plain asset's unit is worth `scale` base units forever. A yield asset's is
/// worth `gross / supply`: the pool's holding grows with its venue while the
/// unit count does not, so the same note is worth more later. `scale` still
/// governs an empty pool, where there is no ratio yet and one unit is worth
/// exactly `scale` by definition — which is what pins a new asset's index.
///
/// The ratio is used directly rather than via the reported index. The index is
/// `gross * RAY / (supply * scale)`, in which `scale` and `RAY` both cancel out
/// of a conversion; going through it would divide and re-multiply by a rounded
/// value and disagree with the pool by a unit at the boundary.
///
/// Same asymmetry as [`Scale`], and for the same reason: dividing down must
/// round up or a quote underpays, and multiplying up must round down or the
/// relayer credits more than the pool would actually hand over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    scale: Scale,
    gross: U256,
    supply: U256,
}

impl Rate {
    /// An asset with no venue: the rate is its scale and never moves.
    pub fn plain(scale: Scale) -> Self {
        Self {
            scale,
            gross: U256::ZERO,
            supply: U256::ZERO,
        }
    }

    /// A yield asset, at the `gross` and `supply` the pool last reported.
    ///
    /// Either being zero means nothing is outstanding yet, which is the empty
    /// pool: the rate falls back to `scale`, matching the contract, and the
    /// zero denominator never reaches a division.
    pub fn yielding(scale: Scale, gross: U256, supply: U256) -> Self {
        Self {
            scale,
            gross,
            supply,
        }
    }

    /// What `circuit_value` is worth in ERC-20 base units, rounded down.
    ///
    /// Falls back to `scale` only when nothing is outstanding, matching
    /// `YieldOps._toUnderlying`, whose sole fallback is `s == 0`. A supply with
    /// zero `gross` is a different thing — a pool whose venue lost everything —
    /// and is worth zero here exactly as it is on chain. Treating that as an
    /// empty pool would credit worthless units at face value.
    pub fn to_base(self, circuit_value: u128) -> U256 {
        if self.supply.is_zero() {
            return self.scale.to_base(circuit_value);
        }
        U256::from(circuit_value) * self.gross / self.supply
    }

    /// The smallest whole note value worth at least `base` base units.
    ///
    /// `U256::MAX` when units are outstanding against no backing: no finite
    /// number of worthless units covers a cost, and the callers saturate that
    /// into a bar no payer can clear. Falling back to `scale` instead would
    /// quote cheaply against units the pool would pay nothing for.
    pub fn to_circuit_ceil(self, base: U256) -> U256 {
        if self.supply.is_zero() {
            return self.scale.to_circuit_ceil(base);
        }
        if self.gross.is_zero() {
            return U256::MAX;
        }
        let numer = base * self.supply;
        (numer + self.gross - U256::from(1u8)) / self.gross
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

#[cfg(test)]
mod rate_tests {
    use super::*;
    use std::str::FromStr;

    fn scale(s: &str) -> Scale {
        Scale::from_decimal(&BigDecimal::from_str(s).expect("decimal")).expect("usable scale")
    }

    /// An empty pool has no ratio yet, so a unit is worth exactly `scale` and
    /// the rate is indistinguishable from a plain asset's.
    #[test]
    fn an_empty_pool_prices_at_scale() {
        let r = Rate::yielding(scale("1000"), U256::ZERO, U256::ZERO);
        assert_eq!(r.to_base(5), Rate::plain(scale("1000")).to_base(5));
        assert_eq!(r.to_base(5), U256::from(5000u16));
    }

    /// The whole point: once the venue has earned, a unit is worth more than
    /// `scale`, and a quote for a fixed cost needs fewer of them.
    #[test]
    fn a_grown_index_makes_a_unit_worth_more() {
        let plain = Rate::plain(scale("1000"));
        // supply 1_000 units backed by 1_100_000 base: 10% above `scale`.
        let grown = Rate::yielding(
            scale("1000"),
            U256::from(1_100_000u32),
            U256::from(1_000u32),
        );

        assert!(grown.to_base(100) > plain.to_base(100));
        assert_eq!(grown.to_base(100), U256::from(110_000u32));

        // …and covering a fixed cost therefore takes fewer units, which is the
        // direction the pre-`Rate` code got backwards.
        let cost = U256::from(110_000u32);
        assert!(grown.to_circuit_ceil(cost) < plain.to_circuit_ceil(cost));
    }

    /// The property `Scale` already guarantees, which is the one a yield asset
    /// broke: a quote rounded up must never come back worth less than the
    /// amount it was quoted for.
    #[test]
    fn a_rounded_up_note_always_covers_the_amount_it_came_from() {
        let rates = [
            Rate::plain(scale("1000")),
            Rate::yielding(
                scale("1000"),
                U256::from(1_100_000u32),
                U256::from(1_000u32),
            ),
            // A deliberately awkward ratio, to exercise the rounding.
            Rate::yielding(scale("1"), U256::from(333_331u32), U256::from(99_991u32)),
            // Below `scale`, as after a venue loss.
            Rate::yielding(scale("1000"), U256::from(900_000u32), U256::from(1_000u32)),
        ];
        for r in rates {
            for base in [1u64, 7, 999, 1000, 1001, 123_456, 999_983] {
                let note: u128 = r.to_circuit_ceil(U256::from(base)).to();
                assert!(
                    r.to_base(note) >= U256::from(base),
                    "{r:?} underpaid for base {base}"
                );
            }
        }
    }

    /// A total loss is not an empty pool. The contract's only fallback is
    /// `supply == 0`, so units backed by nothing are worth nothing — pricing
    /// them at `scale` would credit a worthless payment at face value.
    #[test]
    fn units_with_no_backing_left_are_worth_nothing() {
        let dead = Rate::yielding(scale("1000"), U256::ZERO, U256::from(1_000u32));
        assert_eq!(dead.to_base(100), U256::ZERO);
        // And no finite number of them covers a cost.
        assert_eq!(dead.to_circuit_ceil(U256::from(1u8)), U256::MAX);
    }

    /// A venue loss puts the index below `scale`; nothing about the direction
    /// of rounding changes.
    #[test]
    fn a_loss_is_priced_without_special_casing() {
        let shrunk = Rate::yielding(scale("1000"), U256::from(900_000u32), U256::from(1_000u32));
        assert_eq!(shrunk.to_base(100), U256::from(90_000u32));
        assert!(shrunk.to_circuit_ceil(U256::from(90_000u32)) >= U256::from(100u8));
    }
}
