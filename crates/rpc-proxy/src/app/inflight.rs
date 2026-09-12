//! Calls in flight, by key, so concurrent askers share one upstream call.
//!
//! Used in place of moka's `try_get_with`, which coalesces one key per future:
//! a batch of twenty misses cannot sit in twenty of those and still go upstream
//! as one round trip. Here a caller claims every key it is missing at once,
//! forwards the ones it leads as a single batch, and waits on the ones someone
//! else already has in flight — so batching and coalescing apply to the same
//! call.
//!
//! In-process, like the caches it fronts.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::watch;

/// Keys currently being fetched, each with a channel its answer arrives on.
pub struct InFlight<K, V> {
    calls: Mutex<Calls<K, V>>,
}

type Calls<K, V> = HashMap<K, watch::Receiver<Option<V>>>;

/// The outcome of claiming a key.
pub enum Claim<'a, K: Hash + Eq, V> {
    /// Nobody else is fetching it: this caller must, and [`Lead::finish`] it.
    Lead(Lead<'a, K, V>),
    /// Someone else already is: wait on them.
    Follow(Follow<V>),
}

/// The obligation to fetch one key and publish what came back.
///
/// Dropping it without [`Self::finish`] releases the key with no answer, which
/// is what a cancelled request does: the handler future is dropped when its
/// client disconnects, mid-upstream-call. Followers then see [`Follow::wait`]
/// return `None` and claim the key again, so one of them takes over rather
/// than every one of them failing.
pub struct Lead<'a, K: Hash + Eq, V> {
    key: K,
    tx: watch::Sender<Option<V>>,
    owner: &'a InFlight<K, V>,
}

/// A wait on another caller's fetch.
pub struct Follow<V>(watch::Receiver<Option<V>>);

impl<K, V> InFlight<K, V> {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(HashMap::new()),
        }
    }

    /// Recovers from poisoning rather than propagating it: every critical
    /// section here is a single map operation, so a panic elsewhere cannot have
    /// left the map half-updated.
    fn lock(&self) -> MutexGuard<'_, Calls<K, V>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<K: Hash + Eq + Clone, V> InFlight<K, V> {
    /// Lead `key` if nobody is fetching it, otherwise follow whoever is.
    ///
    /// Synchronous, and the lock is never held across an await, so a caller
    /// claims all of its keys before any of its own fetches begins.
    pub fn claim(&self, key: K) -> Claim<'_, K, V> {
        match self.lock().entry(key) {
            Entry::Occupied(e) => Claim::Follow(Follow(e.get().clone())),
            Entry::Vacant(e) => {
                let (tx, rx) = watch::channel(None);
                let key = e.key().clone();
                e.insert(rx);
                Claim::Lead(Lead {
                    key,
                    tx,
                    owner: self,
                })
            }
        }
    }
}

impl<K, V> Default for InFlight<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Hash + Eq, V> Lead<'_, K, V> {
    /// Hand `value` to every follower, then release the key.
    ///
    /// The caller stores a cacheable value *before* finishing. A caller arriving
    /// after the release then finds it in the cache; one arriving before still
    /// finds the key claimed and follows.
    pub fn finish(self, value: V) {
        self.tx.send_replace(Some(value));
    }
}

impl<K: Hash + Eq, V> Drop for Lead<'_, K, V> {
    fn drop(&mut self) {
        // Only this lead can have inserted the entry, and nobody else can
        // replace it while it is present, so the entry removed is this one.
        self.owner.lock().remove(&self.key);
    }
}

impl<V: Clone> Follow<V> {
    /// The leader's value, or `None` if it released the key without one.
    ///
    /// A value published just before the leader went away is still returned:
    /// the current value is read before the channel's closure is.
    pub async fn wait(mut self) -> Option<V> {
        self.0
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|v| v.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead(f: &InFlight<u8, u32>, k: u8) -> Lead<'_, u8, u32> {
        match f.claim(k) {
            Claim::Lead(l) => l,
            Claim::Follow(_) => panic!("expected to lead {k}"),
        }
    }

    fn follow(f: &InFlight<u8, u32>, k: u8) -> Follow<u32> {
        match f.claim(k) {
            Claim::Follow(w) => w,
            Claim::Lead(_) => panic!("expected to follow {k}"),
        }
    }

    /// The herd case: everyone after the first asker waits on the first.
    #[tokio::test]
    async fn a_second_claim_follows_the_first_and_gets_its_value() {
        let f = InFlight::new();
        let l = lead(&f, 1);
        let w = follow(&f, 1);
        l.finish(42);
        assert_eq!(w.wait().await, Some(42));
    }

    /// Distinct keys never wait on each other.
    #[test]
    fn distinct_keys_are_led_independently() {
        let f = InFlight::<u8, u32>::new();
        let _a = lead(&f, 1);
        let _b = lead(&f, 2);
    }

    /// Once finished the key is free. The next asker must lead — and, since
    /// the leader stored before finishing, find the value in the cache — rather
    /// than follow a fetch that is over.
    #[test]
    fn a_finished_key_is_released() {
        let f = InFlight::new();
        lead(&f, 1).finish(1);
        let _again = lead(&f, 1);
    }

    /// A cancelled leader must not strand its followers: they learn there is no
    /// answer and can claim the key themselves.
    #[tokio::test]
    async fn a_dropped_leader_releases_its_followers_empty_handed() {
        let f = InFlight::<u8, u32>::new();
        let l = lead(&f, 1);
        let w = follow(&f, 1);
        drop(l);
        assert_eq!(w.wait().await, None);
        let _takeover = lead(&f, 1);
    }

    /// A follower that joined before the finish but waits after it still gets
    /// the value, even though the leader is gone by then.
    #[tokio::test]
    async fn a_value_outlives_its_leader() {
        let f = InFlight::new();
        let l = lead(&f, 1);
        let w = follow(&f, 1);
        l.finish(7);
        tokio::task::yield_now().await;
        assert_eq!(w.wait().await, Some(7));
    }
}
