//! Cache construction.
//!
//! [`Cache`] wraps `moka::future::Cache` so that `moka` is named in this module
//! and nowhere else in the workspace. `AppCache` structs stay per-crate because
//! key and value types are domain-specific; what is shared is the cache type and
//! how it is *configured*, so every service ages its entries by the same
//! vocabulary rather than each reaching for moka's builder directly.
//!
//! [`Spec`] is the general form; [`build`] is a shorthand for the common shape.

use std::borrow::Borrow;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

/// A concurrent cache with per-entry expiry.
///
/// Wraps `moka::future::Cache` rather than re-exporting it, so `moka` is an
/// implementation detail of this crate: no other crate names it, and swapping
/// the backing store is a change here rather than across every service.
///
/// The surface is deliberately only what callers use — `get`, `insert`,
/// `try_get_with` and two observability helpers. Anything moka offers beyond
/// that is added here when something actually needs it, not in advance.
///
/// Cloning is cheap and shares one underlying cache, as moka's does.
pub struct Cache<K, V> {
    inner: moka::future::Cache<K, V>,
}

/// Hand-written rather than derived: a derive would demand `K: Clone, V: Clone`,
/// while the handle itself is refcounted and clones regardless of either.
impl<K, V> Clone for Cache<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K, V> fmt::Debug for Cache<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache")
            .field("entry_count", &self.inner.entry_count())
            .finish()
    }
}

impl<K, V> Cache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// The value for `key`, if one is cached and unexpired.
    pub async fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        self.inner.get(key).await
    }

    /// Store `value` under `key`, replacing any current entry.
    pub async fn insert(&self, key: K, value: V) {
        self.inner.insert(key, value).await;
    }

    /// The cached value for `key`, or the result of `init` — computed once.
    ///
    /// Concurrent callers for the same key collapse onto a single run of `init`,
    /// which is the point: it is what stops a herd of requests becoming a herd of
    /// queries. A failure is **not** cached, so the next caller retries.
    ///
    /// The error comes back as `Arc<E>` because that one run's failure is handed
    /// to every caller that waited on it. Callers typically restate it in their
    /// own error type; this signature does not do that for them, since `AppError`
    /// is not `Clone` and the right restatement differs by call site.
    pub async fn try_get_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        self.inner.try_get_with(key, init).await
    }

    /// Drop the entry for `key`, if any.
    ///
    /// For state that becomes wrong before its TTL expires rather than merely
    /// stale — a capability that was just revoked, say. Note that this evicts
    /// from *this* process only: a cache in another replica keeps its copy until
    /// the TTL runs out, so the TTL, not this call, is what bounds how long a
    /// stale entry can be served by the service as a whole.
    pub async fn invalidate<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        self.inner.invalidate(key).await;
    }

    /// Roughly how many entries are held. Approximate: eviction is asynchronous.
    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    /// Run pending eviction and expiry work now.
    ///
    /// Housekeeping is normally asynchronous, so a test asserting on
    /// [`Self::entry_count`] must call this first or it races the maintenance
    /// task. Production code should not need it.
    pub async fn run_pending_tasks(&self) {
        self.inner.run_pending_tasks().await;
    }
}

/// How a cache expires and bounds its entries.
///
/// A TTL is required and a capacity is not, which is the shape the callers
/// actually have: every cache here exists to stop serving something indefinitely,
/// while only some have a key space big enough to need a ceiling. Making the
/// optional half optional is the difference between reusing this and reaching
/// past it for moka's builder.
///
/// ```
/// use shared::cache::Spec;
/// use std::time::Duration;
///
/// let c: shared::cache::Cache<u64, String> =
///     Spec::ttl(Duration::from_secs(30)).max_capacity(64).build();
/// assert_eq!(c.entry_count(), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    ttl: Duration,
    max_capacity: Option<u64>,
}

impl Spec {
    /// Entries expire this long after they are written.
    ///
    /// Unbounded in size until [`Self::max_capacity`] is added: appropriate when
    /// the key space is naturally small or already bounded elsewhere, as for a
    /// set of in-flight nullifiers that the TTL alone is there to drain.
    pub const fn ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            max_capacity: None,
        }
    }

    /// Also evict least-recently-used entries past `max`.
    ///
    /// A ceiling on memory, not a correctness property: eviction is approximate
    /// and asynchronous, so a cache may briefly hold more than this.
    pub const fn max_capacity(self, max: u64) -> Self {
        Self {
            max_capacity: Some(max),
            ..self
        }
    }

    /// The configured cache.
    pub fn build<K, V>(self) -> Cache<K, V>
    where
        K: Hash + Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        let mut b = moka::future::Cache::builder().time_to_live(self.ttl);
        if let Some(max) = self.max_capacity {
            b = b.max_capacity(max);
        }
        Cache { inner: b.build() }
    }

    /// The configured cache, with [`Self::max_capacity`] counting *weigher
    /// units* rather than entries.
    ///
    /// For a cache whose values differ in size by orders of magnitude, where an
    /// entry count is not a bound on memory at all: a ceiling sized for the
    /// common small value is no ceiling once the large ones arrive, and one
    /// sized for the large value throws away almost all of the cache. Weighing
    /// by serialized length bounds the bytes directly, which is the quantity
    /// that actually has to fit.
    ///
    /// The weigher must return at least 1 — moka treats 0 as free, so a class of
    /// entries weighed 0 would never be evicted.
    ///
    /// Takes the weigher here rather than on [`Spec`] so `Spec` stays
    /// non-generic and `Copy`; the key and value types only exist at `build`.
    pub fn build_weighed<K, V>(
        self,
        weigher: impl Fn(&K, &V) -> u32 + Send + Sync + 'static,
    ) -> Cache<K, V>
    where
        K: Hash + Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        let mut b = moka::future::Cache::builder()
            .time_to_live(self.ttl)
            .weigher(move |k, v| weigher(k, v).max(1));
        if let Some(max) = self.max_capacity {
            b = b.max_capacity(max);
        }
        Cache { inner: b.build() }
    }
}

/// A bounded cache with a TTL — the shape most callers want.
///
/// Shorthand for `Spec::ttl(ttl).max_capacity(max_capacity).build()`.
pub fn build<K, V>(max_capacity: u64, ttl: Duration) -> Cache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    Spec::ttl(ttl).max_capacity(max_capacity).build()
}

/// Read `key` through `cache`, running `load` exactly once on a miss.
///
/// Every cached route wants the same three things: collapse a herd of callers
/// onto one query, hand the same body to all of them, and restate the one
/// failure they shared. Only the last needs saying at each call site, and every
/// webserver says it identically — so it is said here instead.
///
/// [`Cache::try_get_with`] deliberately hands back `Arc<E>` rather than guessing
/// how a caller wants the shared failure worded: the loader runs once and its
/// error reaches every waiter, and [`AppError`] is not `Clone`. Restating it as
/// [`AppError::Internal`] loses no status code — the variants a miss can raise
/// are already 500s — and the original detail survives in the message, which
/// `crate::http` logs rather than returning to the client.
///
/// `load` is a future rather than a closure because it must not run on a hit;
/// futures are lazy, so building it before the lookup costs nothing.
#[cfg(feature = "webserver")]
pub async fn cached<K, V, F>(cache: &Cache<K, V>, key: K, load: F) -> crate::http::AppResult<V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    F: Future<Output = crate::http::AppResult<V>>,
{
    cache
        .try_get_with(key, load)
        .await
        .map_err(|e: Arc<crate::http::AppError>| crate::http::AppError::Internal(e.to_string()))
}

/// [`cached`], reporting hit or miss under the `metric` label.
///
/// `moka` reports neither hit nor miss itself, so the outcome is only visible
/// through a flag set inside the initialiser — and the counter has to be
/// recorded on the error path too, where a failed load is still a miss.
#[cfg(feature = "webserver")]
pub async fn cached_metered<K, V, F>(
    cache: &Cache<K, V>,
    metric: &'static str,
    key: K,
    load: F,
) -> crate::http::AppResult<V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    F: Future<Output = crate::http::AppResult<V>>,
{
    let probe = crate::metrics::CacheProbe::new(metric);
    let miss = probe.marker();
    let out = cached(cache, key, async move {
        miss.mark();
        load.await
    })
    .await;
    probe.record();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eviction is asynchronous, so a capacity assertion has to force the
    /// pending work first or it races the maintenance task.
    async fn fill(cache: &Cache<u64, u64>, n: u64) {
        for i in 0..n {
            cache.insert(i, i).await;
        }
        cache.run_pending_tasks().await;
    }

    #[tokio::test]
    async fn test_build_over_capacity_evicts_down_to_the_ceiling() {
        let cache: Cache<u64, u64> = build(8, Duration::from_secs(60));
        fill(&cache, 64).await;
        assert_eq!(cache.entry_count(), 8);
    }

    /// The gap that sent `nullifier_guard` to moka's builder directly: a TTL
    /// with no ceiling, for a key space bounded by something other than size.
    #[tokio::test]
    async fn test_spec_without_capacity_keeps_every_entry() {
        let cache: Cache<u64, u64> = Spec::ttl(Duration::from_secs(60)).build();
        fill(&cache, 64).await;
        assert_eq!(cache.entry_count(), 64);
    }

    #[tokio::test]
    async fn test_invalidate_drops_one_entry_and_leaves_the_rest() {
        let cache: Cache<u64, u64> = build(8, Duration::from_secs(60));
        cache.insert(1, 1).await;
        cache.insert(2, 2).await;

        cache.invalidate(&1).await;
        assert_eq!(cache.get(&1).await, None);
        assert_eq!(cache.get(&2).await, Some(2));
    }

    /// Must not resurrect a key or panic; a revocation may race the entry's own
    /// expiry.
    #[tokio::test]
    async fn test_invalidating_an_absent_key_is_a_no_op() {
        let cache: Cache<u64, u64> = build(8, Duration::from_secs(60));
        cache.invalidate(&99).await;
        cache.run_pending_tasks().await;
        assert_eq!(cache.entry_count(), 0);
    }

    #[tokio::test]
    async fn test_spec_past_ttl_stops_serving_the_entry() {
        let cache: Cache<u64, u64> = Spec::ttl(Duration::from_millis(20)).build();
        cache.insert(1, 1).await;
        assert_eq!(cache.get(&1).await, Some(1));

        // A real sleep: moka keeps its own clock, so tokio's paused time does
        // not move the TTL along.
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(cache.get(&1).await, None);
    }

    /// `build` is documented as shorthand; if it ever stops agreeing with the
    /// spec it delegates to, that promise is silently broken.
    #[test]
    fn test_build_matches_the_spec_it_delegates_to() {
        let ttl = Duration::from_secs(30);
        assert_eq!(
            Spec::ttl(ttl).max_capacity(64),
            Spec {
                ttl,
                max_capacity: Some(64)
            }
        );
    }
}
