//! Cache-fronted price lookup shared by every service that reports USD.

use crate::llama::{PriceClient, TokenKey, TokenPrice};
use moka::future::Cache;
use std::collections::HashMap;
use tracing::warn;

/// Where resolved prices are kept between requests.
///
/// `None` records a token the provider could not price. Caching that answer is
/// the point: without it every request re-asks upstream about tokens that will
/// never have a price.
pub type PriceCache = Cache<TokenKey, Option<TokenPrice>>;

/// Resolve USD prices for `keys`, serving what the cache holds and fetching
/// the rest in a single upstream request.
///
/// Never fails: prices decorate data that is useful without them, so a dead
/// provider degrades those fields to absent rather than failing the whole
/// endpoint. A negative result is cached like any other so an unpriceable token
/// is asked about once per TTL, not once per request; a *failed* fetch is not
/// cached, so a transient outage retries.
pub async fn for_tokens(
    client: &PriceClient,
    cache: &PriceCache,
    keys: &[TokenKey],
) -> HashMap<TokenKey, TokenPrice> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();

    for key in keys {
        match cache.get(key).await {
            Some(cached) => {
                if let Some(price) = cached {
                    out.insert(key.clone(), price);
                }
            }
            None => missing.push(key.clone()),
        }
    }
    if missing.is_empty() {
        return out;
    }

    match client.fetch(&missing).await {
        Ok(fetched) => {
            for key in missing {
                let price = fetched.get(&key).copied();
                cache.insert(key.clone(), price).await;
                if let Some(price) = price {
                    out.insert(key, price);
                }
            }
        }
        Err(e) => warn!(error = %e, tokens = missing.len(), "price fetch failed; omitting USD"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(chain_id: i64, hex: &str) -> TokenKey {
        (chain_id, hex.to_string())
    }

    fn price(price_usd: f64) -> TokenPrice {
        TokenPrice {
            price_usd,
            decimals: Some(18),
            quoted_at: 1,
        }
    }

    /// Refuses every connection immediately, so a test that reaches the network
    /// fails fast and deterministically instead of waiting on DNS.
    fn unreachable_client() -> PriceClient {
        PriceClient::new("http://127.0.0.1:1".into(), Duration::from_millis(200)).unwrap()
    }

    fn cache() -> PriceCache {
        Cache::builder()
            .max_capacity(16)
            .time_to_live(Duration::from_secs(60))
            .build()
    }

    #[tokio::test]
    async fn a_fully_cached_set_never_calls_upstream() {
        // The client cannot reach anything; the answer must come from the cache
        // alone, which is what keeps a poll off the provider.
        let c = cache();
        let k = key(1, "a0b8");
        c.insert(k.clone(), Some(price(2.0))).await;

        let got = for_tokens(&unreachable_client(), &c, std::slice::from_ref(&k)).await;
        assert_eq!(got.get(&k).unwrap().price_usd, 2.0);
    }

    #[tokio::test]
    async fn a_cached_negative_is_omitted_without_asking_again() {
        let c = cache();
        let k = key(1, "dead");
        c.insert(k.clone(), None).await;

        assert!(for_tokens(&unreachable_client(), &c, &[k]).await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_fetch_omits_usd_and_is_not_cached() {
        // Not caching the failure is what lets a transient outage retry on the
        // next request rather than serving "unpriced" for a whole TTL.
        let c = cache();
        let k = key(1, "a0b8");

        assert!(
            for_tokens(&unreachable_client(), &c, std::slice::from_ref(&k))
                .await
                .is_empty()
        );
        assert!(
            c.get(&k).await.is_none(),
            "a failed fetch must leave no entry"
        );
    }

    #[tokio::test]
    async fn a_token_on_an_unsupported_chain_is_answered_without_a_request() {
        // `fetch` makes no request when nothing maps to a provider slug, so this
        // resolves to "unpriced" even though the client is unreachable — the
        // reason the local anvil stack stays offline.
        let c = cache();
        let k = key(31337, "a0b8");

        assert!(
            for_tokens(&unreachable_client(), &c, std::slice::from_ref(&k))
                .await
                .is_empty()
        );
        // It *is* cached: an all-unsupported batch is a successful empty fetch.
        assert_eq!(c.get(&k).await, Some(None));
    }
}
