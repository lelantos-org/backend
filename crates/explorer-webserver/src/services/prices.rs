use crate::adapters::{TokenKey, TokenPrice};
use crate::app::AppState;
use std::collections::HashMap;
use tracing::warn;

/// Resolve USD prices for `keys`, serving what the cache holds and fetching
/// the rest in a single upstream request.
///
/// Never fails: prices decorate public chain data, so a dead provider degrades
/// those fields to absent rather than failing the whole endpoint. A negative
/// result is cached like any other so an unpriceable token is asked about once
/// per TTL, not once per request; a *failed* fetch is not cached, so a
/// transient outage retries.
pub async fn for_tokens(st: &AppState, keys: Vec<TokenKey>) -> HashMap<TokenKey, TokenPrice> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();

    for key in keys {
        match st.cache.prices.get(&key).await {
            Some(cached) => {
                if let Some(price) = cached {
                    out.insert(key, price);
                }
            }
            None => missing.push(key),
        }
    }
    if missing.is_empty() {
        return out;
    }

    match st.prices.fetch(&missing).await {
        Ok(fetched) => {
            for key in missing {
                let price = fetched.get(&key).copied();
                st.cache.prices.insert(key.clone(), price).await;
                if let Some(price) = price {
                    out.insert(key, price);
                }
            }
        }
        Err(e) => warn!(error = %e, tokens = missing.len(), "price fetch failed; omitting USD"),
    }
    out
}

/// Convert a token-base-unit amount to USD. `None` when the provider gave a
/// price but no decimals — without them the amount has no known magnitude.
pub fn to_usd(base_units: f64, price: &TokenPrice) -> Option<f64> {
    let decimals = price.decimals?;
    Some(base_units / 10f64.powi(decimals as i32) * price.price_usd)
}

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
        let usd = to_usd(1_500_000.0, &price(0.99, Some(6))).unwrap();
        assert!((usd - 1.485).abs() < 1e-9, "got {usd}");
    }

    #[test]
    fn eighteen_decimals_do_not_overflow_f64() {
        let usd = to_usd(2e18, &price(3000.0, Some(18))).unwrap();
        assert!((usd - 6000.0).abs() < 1e-6, "got {usd}");
    }

    #[test]
    fn refuses_to_convert_without_decimals() {
        assert_eq!(to_usd(1e18, &price(3000.0, None)), None);
    }
}
