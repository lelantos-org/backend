use bigdecimal::BigDecimal;

/// Whole tokens for a base-unit amount.
///
/// `None` when the indexer has not resolved the token's decimals — unknown, so
/// no amount is reported rather than a wrong one. Never use `assets.scale` for
/// this: `scale` sizes a value for the circuit (`baseUnits / scale` must fit
/// `uint48`), so circuit units per whole token is `10^decimals / scale` and
/// varies per asset.
pub fn whole_tokens(base: &BigDecimal, decimals: Option<i16>) -> Option<BigDecimal> {
    let decimals = decimals.filter(|d| *d >= 0)?;
    // BigDecimal carries its own scale, so this is exact: 14e18 base at 18
    // decimals is exactly 14, with no rounding.
    Some(base / BigDecimal::new(1.into(), -i64::from(decimals)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn bd(s: &str) -> BigDecimal {
        BigDecimal::from_str(s).unwrap()
    }

    #[test]
    fn converts_exactly() {
        let got = whole_tokens(&bd("14000000000000000000"), Some(18)).unwrap();
        assert_eq!(got.normalized().to_string(), "14");
    }

    #[test]
    fn keeps_fractions() {
        let got = whole_tokens(&bd("150000000"), Some(8)).unwrap();
        assert_eq!(got.normalized().to_string(), "1.5");
    }

    #[test]
    fn zero_decimals_is_identity() {
        assert_eq!(
            whole_tokens(&bd("7"), Some(0))
                .unwrap()
                .normalized()
                .to_string(),
            "7"
        );
    }

    #[test]
    fn unknown_decimals_yield_nothing() {
        assert!(whole_tokens(&bd("1"), None).is_none());
        assert!(whole_tokens(&bd("1"), Some(-1)).is_none());
    }
}
