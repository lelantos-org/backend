//! Response caches.
//!
//! Keyed by the query that produced them rather than by row, so a herd of wallet
//! polls collapses onto one database read. Key and value types are
//! domain-specific, which is why this struct is per-crate; the construction and
//! expiry vocabulary is [`shared::cache`].

use crate::domain::responses::{AssetOut, PricesResponse, YieldIndexResponse};
use shared::cache::Cache;
use std::sync::Arc;
use std::time::Duration;

/// Room for every chain served, plus the all-chains query, with slack.
const ASSETS_CAPACITY: u64 = 64;

/// How long one `/v1/prices` body is reused.
///
/// Shorter than `token_prices.ttl_s`, and not tied to `cache_ttl_s` the way the
/// catalog is: this bounds how long a price that has already refreshed upstream
/// stays invisible, while the provider TTL bounds how often upstream is asked at
/// all. The catalog's TTL answers a different question — how stale a registered
/// asset may be — and a price moves faster than that.
const PRICES_TTL: Duration = Duration::from_secs(30);

/// One entry per chain served, with slack.
const YIELD_INDEX_CAPACITY: u64 = 64;

/// How long one index-history body is reused.
///
/// Its own TTL rather than the catalog's: the sampler writes every 30 minutes,
/// so anything shorter re-reads rows that cannot have changed. Half the sampling
/// interval bounds how long a new reading stays invisible while still collapsing
/// the polls in between onto one query.
const YIELD_INDEX_TTL_S: u64 = 15 * 60;

#[derive(Clone)]
pub struct AppCache {
    /// `None` is the all-chains query; `Some(id)` one chain's.
    pub assets: Cache<Option<i64>, Arc<Vec<AssetOut>>>,
    /// The whole `/v1/prices` body, under the unit key. One entry, because the
    /// route takes no parameters: every caller gets every chain.
    pub prices: Cache<(), Arc<PricesResponse>>,
    /// One chain's index history. Keyed by chain because the route serves every
    /// asset on it in one body.
    pub yield_index: Cache<i64, Arc<YieldIndexResponse>>,
}

impl AppCache {
    pub fn new(ttl_s: u64) -> Self {
        Self {
            assets: shared::cache::build(ASSETS_CAPACITY, Duration::from_secs(ttl_s.max(1))),
            prices: shared::cache::build(1, PRICES_TTL),
            yield_index: shared::cache::build(
                YIELD_INDEX_CAPACITY,
                Duration::from_secs(YIELD_INDEX_TTL_S),
            ),
        }
    }
}
