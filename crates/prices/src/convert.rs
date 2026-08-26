//! Base-units → USD conversion.

use crate::llama::TokenPrice;

/// Convert a token-base-unit amount to USD.
///
/// `asset_decimals` is the token's own `decimals()` as the indexer read it and
/// takes precedence over the provider's, which reports decimals as metadata
/// about a price feed; a disagreement puts the dollar figure off by a power of
/// ten. The provider's value is the fallback for an asset the indexer has not
/// backfilled yet, so a priced token still contributes to a dollar total.
///
/// `None` when neither source knows the magnitude of a base unit, or when the
/// reported value is outside the range an ERC20 can have.
pub fn to_usd(base_units: f64, asset_decimals: Option<i16>, price: &TokenPrice) -> Option<f64> {
    let decimals = plausible_decimals(asset_decimals.map(i32::from))
        .or_else(|| plausible_decimals(price.decimals.and_then(|d| i32::try_from(d).ok())))?;
    Some(base_units / 10f64.powi(decimals) * price.price_usd)
}

/// `decimals()` is a `uint8` in ERC20. The upper bound keeps a corrupt value
/// from dividing a real amount down to `$0.00`, which would read as a
/// measurement rather than as missing data.
fn plausible_decimals(decimals: Option<i32>) -> Option<i32> {
    decimals.filter(|d| (0..=MAX_DECIMALS).contains(d))
}

const MAX_DECIMALS: i32 = 38;

#[cfg(test)]
mod tests {
    use super::*;

    fn price(price_usd: f64, decimals: Option<u32>) -> TokenPrice {
        TokenPrice {
            price_usd,
            decimals,
            quoted_at: 0,
        }
    }

    #[test]
    fn converts_base_units_through_decimals() {
        // 1_500_000 base units of a 6-decimal token at $0.99 = 1.5 tokens.
        let usd = to_usd(1_500_000.0, Some(6), &price(0.99, Some(6))).unwrap();
        assert!((usd - 1.485).abs() < 1e-9, "got {usd}");
    }

    #[test]
    fn eighteen_decimals_do_not_overflow_f64() {
        let usd = to_usd(2e18, Some(18), &price(3000.0, Some(18))).unwrap();
        assert!((usd - 6000.0).abs() < 1e-6, "got {usd}");
    }

    #[test]
    fn the_tokens_own_decimals_win_over_the_providers() {
        // The provider claims 18 for a 6-decimal token; trusting it would report
        // $1.5e-12 for 1.5 USDC.
        let usd = to_usd(1_500_000.0, Some(6), &price(1.0, Some(18))).unwrap();
        assert!((usd - 1.5).abs() < 1e-9, "got {usd}");
    }

    #[test]
    fn the_provider_covers_an_asset_we_have_not_backfilled() {
        let usd = to_usd(2e18, None, &price(3000.0, Some(18))).unwrap();
        assert!((usd - 6000.0).abs() < 1e-6, "got {usd}");
    }

    #[test]
    fn refuses_to_convert_when_neither_source_knows_the_decimals() {
        assert_eq!(to_usd(1e18, None, &price(3000.0, None)), None);
    }

    #[test]
    fn a_negative_decimals_column_is_not_a_magnitude() {
        assert_eq!(to_usd(1e18, Some(-1), &price(3000.0, None)), None);
    }

    #[test]
    fn an_implausible_decimals_value_reports_nothing_rather_than_zero_dollars() {
        // 10^300 would divide a real amount down to $0.00, which would read as
        // a measured zero.
        assert_eq!(to_usd(1e18, Some(300), &price(3000.0, None)), None);
        assert_eq!(to_usd(1e18, None, &price(3000.0, Some(300))), None);
    }

    #[test]
    fn an_implausible_column_still_falls_back_to_the_provider() {
        let usd = to_usd(2e18, Some(300), &price(3000.0, Some(18))).unwrap();
        assert!((usd - 6000.0).abs() < 1e-6, "got {usd}");
    }
}
