//! Cache-fronted price lookup shared by every service that reports USD.

use crate::providers::PriceProvider;
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
    /// served before upstream is asked again. It is floored at `MIN_TTL`.
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
mod tests;
