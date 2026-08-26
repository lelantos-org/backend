use bigdecimal::BigDecimal;

/// Whole tokens for a base-unit amount.
///
/// `None` when the indexer has not resolved the token's decimals, so no amount
/// is reported rather than a wrong one. `assets.scale` is not a substitute: it
/// sizes a value for the circuit (`baseUnits / scale` must fit `uint48`), so
/// circuit units per whole token is `10^decimals / scale` and varies per asset.
pub fn whole_tokens(base: &BigDecimal, decimals: Option<i16>) -> Option<BigDecimal> {
    let decimals = decimals.filter(|d| *d >= 0)?;
    // BigDecimal carries its own scale, so this is exact: 14e18 base units at
    // 18 decimals is exactly 14, without rounding.
    Some(base / BigDecimal::new(1.into(), -i64::from(decimals)))
}

/// The wire form of `whole_tokens`: trailing zeros trimmed, always plain
/// decimal.
///
/// `to_string()` switches to scientific notation for small magnitudes — a single
/// wei of an 18-decimal token prints as `2E-18` — and an amount field whose
/// syntax varies with its magnitude breaks clients that do more than `Number()`
/// on it. Every endpoint that reports token amounts uses this, so the format is
/// uniform.
pub fn whole_tokens_str(base: &BigDecimal, decimals: Option<i16>) -> Option<String> {
    whole_tokens(base, decimals).as_ref().map(plain_amount)
}

/// The same wire form for an amount that is already in whole tokens.
pub fn plain_amount(amount: &BigDecimal) -> String {
    amount.normalized().to_plain_string()
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

    #[test]
    fn the_wire_form_trims_zeros() {
        assert_eq!(
            whole_tokens_str(&bd("14000000000000000000"), Some(18)).as_deref(),
            Some("14")
        );
        assert_eq!(
            whole_tokens_str(&bd("150000000"), Some(8)).as_deref(),
            Some("1.5")
        );
    }

    #[test]
    fn the_wire_form_stays_decimal_for_a_single_wei() {
        // `to_string()` would emit "2E-18" here, varying the field's syntax with
        // its magnitude.
        assert_eq!(
            whole_tokens_str(&bd("2"), Some(18)).as_deref(),
            Some("0.000000000000000002")
        );
    }

    #[test]
    fn the_wire_form_reports_nothing_without_decimals() {
        assert_eq!(whole_tokens_str(&bd("1"), None), None);
    }
}
