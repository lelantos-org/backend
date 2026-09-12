//! Suppressing repeats of a log line that callers can cause.
//!
//! A WARN that fires per request is a cost any caller can impose: send the same
//! refused call in a loop and this service writes a line for every one, and
//! the single line an operator needed is buried under a million copies of
//! itself. Metrics still count every occurrence; the log says once per window
//! that it is happening.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Admits the first line per key per window.
pub struct LogThrottle<K> {
    window: Duration,
    /// Most distinct keys remembered. A caller who can pick the key — a new
    /// contract address per call — must not be able to grow this without
    /// bound; see [`Self::admit`].
    capacity: usize,
    last: Mutex<HashMap<K, Instant>>,
}

impl<K: Hash + Eq> LogThrottle<K> {
    pub fn new(window: Duration, capacity: usize) -> Self {
        Self {
            window,
            capacity,
            last: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a line for `key` should be written now: `true` the first time
    /// `key` is seen in a window.
    ///
    /// When every remembered key is still inside its window and there is no
    /// room for another, the line is suppressed rather than the map grown. A
    /// flood of distinct keys is itself what the metrics show; losing one of
    /// its lines costs nothing that growing without bound would not cost more.
    pub fn admit(&self, key: K) -> bool {
        self.admit_at(key, Instant::now())
    }

    fn admit_at(&self, key: K, now: Instant) -> bool {
        // Every critical section is one map operation, so a panic elsewhere
        // cannot have left the map inconsistent.
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let live = |at: &Instant| now.saturating_duration_since(*at) < self.window;

        if last.get(&key).is_some_and(live) {
            return false;
        }
        if last.len() >= self.capacity && !last.contains_key(&key) {
            last.retain(|_, at| live(at));
            if last.len() >= self.capacity {
                return false;
            }
        }
        last.insert(key, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(600);

    #[test]
    fn a_repeat_inside_the_window_is_suppressed() {
        let t = LogThrottle::new(WINDOW, 8);
        let now = Instant::now();
        assert!(t.admit_at("a", now));
        assert!(!t.admit_at("a", now + Duration::from_secs(1)));
        assert!(t.admit_at("b", now), "keys are independent");
    }

    /// Suppression is a window, not forever: an allowlist that is still stale
    /// an hour later is still worth a line.
    #[test]
    fn a_repeat_after_the_window_is_written_again() {
        let t = LogThrottle::new(WINDOW, 8);
        let now = Instant::now();
        assert!(t.admit_at("a", now));
        assert!(t.admit_at("a", now + WINDOW));
    }

    /// A caller minting a fresh key per request cannot grow the map. Once full
    /// of live keys, new ones are suppressed.
    #[test]
    fn a_full_throttle_suppresses_rather_than_grows() {
        let t = LogThrottle::new(WINDOW, 2);
        let now = Instant::now();
        assert!(t.admit_at(1, now));
        assert!(t.admit_at(2, now));
        assert!(!t.admit_at(3, now));
        assert_eq!(t.last.lock().unwrap().len(), 2);
    }

    /// Expired keys make room before anything is suppressed.
    #[test]
    fn expired_keys_are_evicted_to_make_room() {
        let t = LogThrottle::new(WINDOW, 2);
        let now = Instant::now();
        assert!(t.admit_at(1, now));
        assert!(t.admit_at(2, now));
        assert!(t.admit_at(3, now + WINDOW));
    }
}
