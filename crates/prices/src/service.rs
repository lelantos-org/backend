//! Cache-fronted price lookup shared by every service that reports USD.

use crate::provider::PriceProvider;
use crate::token::{TokenKey, TokenPrice};
use shared::cache::Cache;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// Where resolved prices are kept between requests.
///
/// `None` records a token no provider could price. Caching that answer stops
/// every request from re-asking upstream about tokens that will never have a
/// price.
type PriceCache = Cache<TokenKey, Option<TokenPrice>>;

/// Room for every registered asset on every chain a deployment serves, with
/// slack. An entry is two `f64`s and an `i64`, so the ceiling is cheap.
const CACHE_CAPACITY: u64 = 1_024;

/// A zero TTL would expire every entry on write, turning the cache into a
/// per-request round trip to upstream. Config carries the TTL in seconds, so
/// this is what an unset or zeroed value lands on.
const MIN_TTL: Duration = Duration::from_secs(1);

/// Prices tokens from an ordered list of providers, in front of one cache.
///
/// The order is a fallback chain, not a race: a token is offered to the next
/// provider only when the ones before it either do not cover its chain, do not
/// know it, or failed. One provider is the common case and costs one pass.
pub struct PriceService {
    providers: Vec<Arc<dyn PriceProvider>>,
    cache: PriceCache,
}

impl PriceService {
    /// `ttl` is how long one token's answer — a price or a "no price" — is
    /// served before upstream is asked again. It is floored at [`MIN_TTL`].
    pub fn new(providers: Vec<Arc<dyn PriceProvider>>, ttl: Duration) -> Self {
        // Not an error: the service still answers, it just answers "unpriced"
        // for everything. Said out loud because that is indistinguishable from a
        // provider outage in the endpoint's output.
        if providers.is_empty() {
            warn!("no price providers configured; every token will report unpriced");
        }
        Self {
            providers,
            cache: shared::cache::build(CACHE_CAPACITY, ttl.max(MIN_TTL)),
        }
    }

    /// Resolve USD prices for `keys`, serving what the cache holds and asking
    /// each provider for the rest in a single request.
    ///
    /// Never fails: prices decorate data that is useful without them, so a dead
    /// provider leaves those fields absent rather than failing the endpoint.
    pub async fn for_tokens(&self, keys: &[TokenKey]) -> HashMap<TokenKey, TokenPrice> {
        let (mut priced, pending) = self.served_from_cache(keys).await;
        if pending.is_empty() {
            return priced;
        }

        let resolved = self.resolve(pending).await;
        // Only what this pass established is written back. Re-inserting a cache
        // hit would push its expiry out by another TTL on every request, so a
        // token asked about often enough would never refresh.
        self.store(&resolved).await;

        priced.extend(resolved.priced);
        priced
    }

    /// Split `keys` into what the cache already answers for and what it does not.
    async fn served_from_cache(
        &self,
        keys: &[TokenKey],
    ) -> (HashMap<TokenKey, TokenPrice>, Vec<TokenKey>) {
        let mut priced = HashMap::new();
        let mut pending = Vec::new();

        for key in unique(keys) {
            match self.cache.get(&key).await {
                Some(Some(price)) => {
                    priced.insert(key, price);
                }
                // A cached "no price": answered, and deliberately omitted.
                Some(None) => {}
                None => pending.push(key),
            }
        }
        (priced, pending)
    }

    /// Walk the provider chain until every token is priced or every provider
    /// that could speak for it has.
    async fn resolve(&self, mut pending: Vec<TokenKey>) -> Resolved {
        let mut resolved = Resolved::default();
        // Tokens whose lookup ended in an error rather than an answer. Held
        // apart so a provider outage is not recorded as "this token has no
        // price".
        let mut failed: HashSet<TokenKey> = HashSet::new();

        for provider in &self.providers {
            if pending.is_empty() {
                break;
            }
            let (mine, rest): (Vec<_>, Vec<_>) = pending
                .into_iter()
                .partition(|k| provider.supports_chain(k.chain));
            pending = rest;
            if mine.is_empty() {
                continue;
            }

            match provider.fetch(&mine).await {
                Ok(fetched) => {
                    for key in mine {
                        match fetched.get(&key) {
                            Some(price) => {
                                resolved.priced.insert(key, *price);
                            }
                            // Unknown here; a later provider may cover it.
                            None => pending.push(key),
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        provider = provider.name(),
                        error = %e,
                        tokens = mine.len(),
                        "price fetch failed; omitting USD"
                    );
                    failed.extend(mine.iter().cloned());
                    pending.extend(mine);
                }
            }
        }

        // What survives the walk is priced by nobody. That is an answer worth
        // caching — unless the only word on the token was a failed request, in
        // which case the next request should ask again rather than wait out a
        // TTL.
        resolved.unpriced = pending
            .into_iter()
            .filter(|k| !failed.contains(k))
            .collect();
        resolved
    }

    async fn store(&self, resolved: &Resolved) {
        for (key, price) in &resolved.priced {
            self.cache.insert(key.clone(), Some(*price)).await;
        }
        for key in &resolved.unpriced {
            self.cache.insert(key.clone(), None).await;
        }
    }
}

/// What one walk of the provider chain established.
///
/// A token that only ever drew an error appears in neither field: it is not an
/// answer, so it is neither returned nor cached.
#[derive(Default)]
struct Resolved {
    priced: HashMap<TokenKey, TokenPrice>,
    /// Asked about and genuinely unknown to every provider that covers it.
    unpriced: Vec<TokenKey>,
}

/// One entry per distinct token, in a stable order.
///
/// Callers key by asset row and several rows can share an ERC-20 — a yield asset
/// is registered alongside the plain asset it shadows. Left in, the repeats
/// would ask a provider about the same token twice in one request.
fn unique(keys: &[TokenKey]) -> Vec<TokenKey> {
    let mut out = keys.to_vec();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::DefiLlama;
    use anyhow::{Result, bail};
    use async_trait::async_trait;
    use shared::chain::ChainId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TEST_TTL: Duration = Duration::from_secs(60);

    fn key(chain_id: i64, hex: &str) -> TokenKey {
        TokenKey::new(chain_id, hex)
    }

    fn price(price_usd: f64) -> TokenPrice {
        TokenPrice {
            price_usd,
            decimals: Some(18),
            quoted_at: 1,
        }
    }

    /// A service whose only provider refuses every connection immediately, so a
    /// test that reaches the network fails fast and deterministically rather
    /// than waiting on DNS.
    fn unreachable() -> PriceService {
        let llama = DefiLlama::new("http://127.0.0.1:1", Duration::from_millis(200)).unwrap();
        PriceService::new(vec![Arc::new(llama)], TEST_TTL)
    }

    /// Answers from a fixed table, or fails outright. Enough to drive the
    /// fallback ordering without a second real upstream.
    #[derive(Default)]
    struct Stub {
        name: &'static str,
        chains: Vec<i64>,
        known: HashMap<TokenKey, TokenPrice>,
        fails: bool,
        calls: AtomicUsize,
        /// Every batch this provider was handed, for asserting what it was — and
        /// was not — asked about.
        asked: std::sync::Mutex<Vec<TokenKey>>,
    }

    impl Stub {
        fn new(name: &'static str, chains: &[i64]) -> Self {
            Self {
                name,
                chains: chains.to_vec(),
                ..Default::default()
            }
        }

        fn prices(mut self, key: TokenKey, usd: f64) -> Self {
            self.known.insert(key, price(usd));
            self
        }

        fn failing(mut self) -> Self {
            self.fails = true;
            self
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        fn asked(&self) -> Vec<TokenKey> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PriceProvider for Stub {
        fn name(&self) -> &'static str {
            self.name
        }

        fn supports_chain(&self, chain: ChainId) -> bool {
            self.chains.contains(&chain.get())
        }

        async fn fetch(&self, tokens: &[TokenKey]) -> Result<HashMap<TokenKey, TokenPrice>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.asked.lock().unwrap().extend_from_slice(tokens);
            if self.fails {
                bail!("{} is down", self.name);
            }
            Ok(tokens
                .iter()
                .filter_map(|k| Some((k.clone(), *self.known.get(k)?)))
                .collect())
        }
    }

    /// The service plus a handle on each stub, since `PriceService` takes them
    /// as `Arc<dyn PriceProvider>` and the assertions need the concrete type.
    fn svc(stubs: Vec<Arc<Stub>>) -> PriceService {
        let providers = stubs
            .into_iter()
            .map(|s| s as Arc<dyn PriceProvider>)
            .collect();
        PriceService::new(providers, TEST_TTL)
    }

    #[tokio::test]
    async fn a_fully_cached_set_never_calls_upstream() {
        // The provider cannot reach anything, so the answer must come from the
        // cache alone.
        let svc = unreachable();
        let k = key(1, "a0b8");
        svc.cache.insert(k.clone(), Some(price(2.0))).await;

        let got = svc.for_tokens(std::slice::from_ref(&k)).await;
        assert_eq!(got.get(&k).unwrap().price_usd, 2.0);
    }

    #[tokio::test]
    async fn a_cached_negative_is_omitted_without_asking_again() {
        let svc = unreachable();
        let k = key(1, "dead");
        svc.cache.insert(k.clone(), None).await;

        assert!(svc.for_tokens(&[k]).await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_fetch_omits_usd_and_is_not_cached() {
        // Not caching the failure lets a transient outage retry on the next
        // request rather than serving "unpriced" for a whole TTL.
        let svc = unreachable();
        let k = key(1, "a0b8");

        assert!(svc.for_tokens(std::slice::from_ref(&k)).await.is_empty());
        assert!(
            svc.cache.get(&k).await.is_none(),
            "a failed fetch must leave no entry"
        );
    }

    #[tokio::test]
    async fn a_token_on_an_unsupported_chain_is_answered_without_a_request() {
        // No provider claims 31337, so this resolves to "unpriced" even though
        // the one configured provider is unreachable. This is why the local
        // anvil stack stays offline.
        let svc = unreachable();
        let k = key(31337, "a0b8");

        assert!(svc.for_tokens(std::slice::from_ref(&k)).await.is_empty());
        // Cached, because "nobody covers this chain" is an answer, not a failure.
        assert_eq!(svc.cache.get(&k).await, Some(None));
    }

    #[tokio::test]
    async fn serving_from_the_cache_does_not_push_the_entry_expiry_out() {
        // The regression this guards: writing every returned price back on every
        // request would reset the TTL, so a token on a polled endpoint would
        // never refresh from upstream.
        // `MIN_TTL`, the shortest the service will hold an entry for, so the
        // sleeps below are as short as this can be written.
        let stub = Arc::new(Stub::new("up", &[1]));
        let svc = PriceService::new(vec![stub.clone()], MIN_TTL);
        let k = key(1, "a0b8");
        svc.cache.insert(k.clone(), Some(price(2.0))).await;

        // Real sleeps: moka keeps its own clock, so tokio's paused time does not
        // move the TTL along. Two thirds of the TTL each, so the entry is still
        // alive for the read and expired by the assertion after it.
        let two_thirds = MIN_TTL.mul_f32(0.7);
        tokio::time::sleep(two_thirds).await;
        assert_eq!(
            svc.for_tokens(std::slice::from_ref(&k)).await[&k].price_usd,
            2.0
        );
        tokio::time::sleep(two_thirds).await;

        assert_eq!(svc.cache.get(&k).await, None, "the TTL was extended");
        assert_eq!(stub.calls(), 0, "the read was served from the cache");
    }

    #[tokio::test]
    async fn a_zero_ttl_is_floored_rather_than_expiring_on_write() {
        let svc = PriceService::new(vec![], Duration::ZERO);
        let k = key(1, "a0b8");
        svc.cache.insert(k.clone(), Some(price(2.0))).await;

        assert_eq!(svc.cache.get(&k).await, Some(Some(price(2.0))));
    }

    #[tokio::test]
    async fn with_no_providers_every_token_is_unpriced() {
        let svc = PriceService::new(vec![], TEST_TTL);
        let k = key(1, "a0b8");

        assert!(svc.for_tokens(std::slice::from_ref(&k)).await.is_empty());
        assert_eq!(svc.cache.get(&k).await, Some(None));
    }

    #[tokio::test]
    async fn one_token_named_twice_is_asked_about_once() {
        // What the callers hand over: a yield asset and the plain asset it
        // shadows are two rows sharing one ERC-20.
        let k = key(1, "a0b8");
        let stub = Arc::new(Stub::new("up", &[1]).prices(k.clone(), 2.0));
        let svc = svc(vec![stub.clone()]);

        let got = svc.for_tokens(&[k.clone(), k.clone(), k.clone()]).await;

        assert_eq!(got.len(), 1);
        assert_eq!(stub.asked(), vec![k], "the repeats must not reach upstream");
    }

    #[tokio::test]
    async fn each_provider_is_asked_only_for_the_chains_it_claims() {
        let mainnet = key(1, "a0b8");
        let base = key(8453, "b1c9");
        let eth = Arc::new(Stub::new("eth-only", &[1]).prices(mainnet.clone(), 2.0));
        let opt = Arc::new(Stub::new("base-only", &[8453]).prices(base.clone(), 3.0));
        let svc = svc(vec![eth.clone(), opt.clone()]);

        let got = svc.for_tokens(&[mainnet.clone(), base.clone()]).await;

        assert_eq!(got.get(&mainnet).unwrap().price_usd, 2.0);
        assert_eq!(got.get(&base).unwrap().price_usd, 3.0);
        assert_eq!(eth.asked(), vec![mainnet]);
        assert_eq!(opt.asked(), vec![base]);
    }

    #[tokio::test]
    async fn a_token_the_first_provider_does_not_know_falls_through_to_the_next() {
        let k = key(1, "a0b8");
        let svc = svc(vec![
            Arc::new(Stub::new("thin", &[1])),
            Arc::new(Stub::new("deep", &[1]).prices(k.clone(), 4.0)),
        ]);

        assert_eq!(
            svc.for_tokens(std::slice::from_ref(&k)).await[&k].price_usd,
            4.0
        );
    }

    #[tokio::test]
    async fn a_provider_is_not_consulted_once_an_earlier_one_priced_the_token() {
        let k = key(1, "a0b8");
        let second = Arc::new(Stub::new("fallback", &[1]).prices(k.clone(), 9.0));
        let svc = svc(vec![
            Arc::new(Stub::new("primary", &[1]).prices(k.clone(), 4.0)),
            second.clone(),
        ]);

        assert_eq!(
            svc.for_tokens(std::slice::from_ref(&k)).await[&k].price_usd,
            4.0
        );
        assert_eq!(second.calls(), 0, "the fallback cost a request anyway");
    }

    #[tokio::test]
    async fn a_failing_provider_is_covered_by_the_next_one() {
        let k = key(1, "a0b8");
        let svc = svc(vec![
            Arc::new(Stub::new("down", &[1]).failing()),
            Arc::new(Stub::new("up", &[1]).prices(k.clone(), 5.0)),
        ]);

        assert_eq!(
            svc.for_tokens(std::slice::from_ref(&k)).await[&k].price_usd,
            5.0
        );
        // The earlier failure must not leave the token marked unpriceable.
        assert_eq!(svc.cache.get(&k).await, Some(Some(price(5.0))));
    }

    #[tokio::test]
    async fn a_token_only_a_failing_provider_covered_stays_uncached() {
        let k = key(1, "a0b8");
        let svc = svc(vec![
            Arc::new(Stub::new("down", &[1]).failing()),
            // Cannot speak for chain 1, so it is never consulted for this token
            // and the failure above is the whole story.
            Arc::new(Stub::new("base-only", &[8453])),
        ]);

        assert!(svc.for_tokens(std::slice::from_ref(&k)).await.is_empty());
        assert!(svc.cache.get(&k).await.is_none());
    }

    #[tokio::test]
    async fn a_token_no_provider_knows_is_cached_as_unpriced() {
        let k = key(1, "dead");
        let svc = svc(vec![Arc::new(Stub::new("thin", &[1]))]);

        assert!(svc.for_tokens(std::slice::from_ref(&k)).await.is_empty());
        assert_eq!(svc.cache.get(&k).await, Some(None));
    }

    #[tokio::test]
    async fn one_batch_mixing_every_outcome_answers_each_token_on_its_own_terms() {
        let priced = key(1, "a0b8");
        let unknown = key(1, "dead");
        let unsupported = key(31337, "beef");
        let svc = svc(vec![Arc::new(
            Stub::new("up", &[1]).prices(priced.clone(), 6.0),
        )]);

        let got = svc
            .for_tokens(&[priced.clone(), unknown.clone(), unsupported.clone()])
            .await;

        assert_eq!(got.len(), 1, "only the priced token comes back");
        assert_eq!(got[&priced].price_usd, 6.0);
        assert_eq!(svc.cache.get(&unknown).await, Some(None));
        assert_eq!(svc.cache.get(&unsupported).await, Some(None));
    }
}
