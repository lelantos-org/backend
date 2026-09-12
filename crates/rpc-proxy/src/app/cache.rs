//! One cache per [`Class`].
//!
//! Separate caches rather than one store with per-entry TTLs: `shared::cache`
//! takes a single time-to-live, and a per-class capacity keeps a burst of tip
//! reads from evicting finalized answers.
//!
//! In-process rather than shared. At N replicas this multiplies upstream
//! traffic by N, which for a one-second head TTL is N requests per second per
//! chain.

use crate::domain::cache_key::CacheKey;
use crate::domain::jsonrpc::Reply;
use crate::domain::policy::Class;
use shared::cache::{Cache, Spec};

type ResultCache = Cache<CacheKey, Reply>;

/// One cache per class.
///
/// Named fields rather than a map: every class is present by construction, so
/// [`Self::get`] is total and needs no fallible lookup.
pub struct Caches {
    head: ResultCache,
    recent: ResultCache,
    finalized: ResultCache,
}

impl Caches {
    pub fn new() -> Self {
        Self {
            head: build(Class::Head),
            recent: build(Class::Recent),
            finalized: build(Class::Finalized),
        }
    }

    pub fn get(&self, class: Class) -> &ResultCache {
        match class {
            Class::Head => &self.head,
            Class::Recent => &self.recent,
            Class::Finalized => &self.finalized,
        }
    }

    /// Current occupancy per class, for the gauge.
    pub fn entry_counts(&self) -> impl Iterator<Item = (Class, u64)> {
        Class::ALL
            .into_iter()
            .map(|c| (c, self.get(c).entry_count()))
    }
}

/// Approximate cost of holding an entry at all: the `CacheKey` plus moka's own
/// node. Counted so a flood of tiny results is bounded too — weighed by body
/// alone, 32 MB of 66-byte `eth_call` answers is half a million entries and far
/// more than 32 MB of actual memory.
const ENTRY_OVERHEAD: u32 = 96;

/// Weighed by serialized length rather than counted; see [`Class::max_bytes`].
fn build(class: Class) -> ResultCache {
    Spec::ttl(class.ttl())
        .max_capacity(class.max_bytes())
        .build_weighed(|_k: &CacheKey, v: &Reply| {
            let body = match v {
                Reply::Result(r) => r.get().len(),
                Reply::Error(e) => e.message.len() + e.data.as_ref().map_or(0, String::len),
            };
            ENTRY_OVERHEAD.saturating_add(body.try_into().unwrap_or(u32::MAX))
        })
}

impl Default for Caches {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every class must map to a cache configured from that class's own TTL and
    /// capacity — a copy-paste in `new` would otherwise give two classes one
    /// lifetime.
    #[test]
    fn every_class_maps_to_its_own_configuration() {
        let c = Caches::new();
        for class in Class::ALL {
            assert_eq!(c.get(class).entry_count(), 0);
        }
        assert_eq!(c.entry_counts().count(), Class::ALL.len());
    }

    /// The classes must not share storage: a two-second answer landing in the
    /// hour-long cache would be served long after it stopped being true.
    #[tokio::test]
    async fn the_classes_are_separate_stores() {
        let caches = Caches::new();
        let key =
            crate::domain::cache_key::key(1, crate::domain::allowlist::Method::EthBlockNumber, &[]);
        let v = Reply::Result(
            serde_json::value::RawValue::from_string("\"0x1\"".into())
                .unwrap()
                .into(),
        );

        caches.get(Class::Head).insert(key, v).await;
        assert!(caches.get(Class::Head).get(&key).await.is_some());
        assert!(caches.get(Class::Finalized).get(&key).await.is_none());
    }

    /// The bound is bytes, not entries.
    ///
    /// `eth_getLogs` results are caller-shaped and can be hundreds of kilobytes
    /// each, so a cache that counted entries would hold 8192 of them. Filling
    /// `Recent` with large values must evict well before its entry count gets
    /// anywhere near what an entry ceiling would have allowed.
    #[tokio::test]
    async fn large_values_evict_on_bytes_rather_than_entry_count() {
        let caches = Caches::new();
        let recent = caches.get(Class::Recent);

        // 256 KB apiece: `Recent` holds 16 MB, so ~64 fit and 400 cannot.
        let big = "0".repeat(256 * 1024);
        for i in 0..400u64 {
            let v = Reply::Result(
                serde_json::value::RawValue::from_string(format!("\"{big}{i}\""))
                    .unwrap()
                    .into(),
            );
            recent
                .insert(
                    crate::domain::cache_key::key(
                        i,
                        crate::domain::allowlist::Method::EthGetLogs,
                        &[],
                    ),
                    v,
                )
                .await;
        }
        recent.run_pending_tasks().await;

        let held = recent.entry_count();
        assert!(
            held < 400,
            "the byte bound must evict; an entry bound would have held all 400, got {held}"
        );
        assert!(
            held * 256 * 1024 <= Class::Recent.max_bytes() + 256 * 1024,
            "held bytes must stay within the class budget, got {held} entries"
        );
    }
}
