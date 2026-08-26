//! Cache builder helper around `moka::future::Cache`.
//!
//! `AppCache` structs stay per-crate because key and value types are
//! domain-specific.

use moka::future::Cache;
use std::hash::Hash;
use std::time::Duration;

pub fn build<K, V>(max_capacity: u64, ttl: Duration) -> Cache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    Cache::builder()
        .max_capacity(max_capacity)
        .time_to_live(ttl)
        .build()
}
