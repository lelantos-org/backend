//! When to rebuild a materialized view.
//!
//! A whole-table aggregate has no incremental form, so a refresh costs
//! O(source rows) however few rows the tick just wrote. Refreshing inline on
//! every tick that committed a matching event makes a catch-up run cost
//! O(rows² / batch): at a 1 s tick and a 500-row batch, several full
//! aggregations per second, per chain, for the whole backfill.
//!
//! The rule is that a refresh happens when the source changed *and* either
//!
//!   - the consumer has caught up, so a reader should see the change now, or
//!   - [`MIN_REFRESH_INTERVAL`] has passed, so a long backfill still publishes
//!     progress rather than going dark until it finishes.
//!
//! Steady-state freshness is unchanged: the first non-saturated tick after a
//! change refreshes, which is the same tick that would have done it inline.
//!
//! This module holds the decision only. Which views exist and how to rebuild
//! them stays with the indexer that owns them — the views are not shared, the
//! rule is.

use std::fmt::Display;
use std::time::{Duration, Instant};
use tracing::warn;

/// Refresh floor while the consumer is still behind.
///
/// Matched to the webservers' default analytic cache TTL: refreshing faster
/// than readers can observe would buy nothing and cost a full aggregation each
/// time.
pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Refresh bookkeeping for one group of views rebuilt together.
///
/// A group, not a view: several views deriving from the same rows share a dirty
/// flag, so one source change is one decision rather than several.
#[derive(Debug)]
pub struct ViewState {
    /// Source rows changed since the last successful refresh.
    dirty: bool,
    /// When a refresh was last *attempted*, successful or not, so a failing
    /// refresh backs off instead of retrying every tick.
    last_attempt: Instant,
    /// Whether that attempt failed. Suppresses the caught-up fast path until
    /// the interval elapses; without it a view whose refresh keeps erroring
    /// would retry on every tick for as long as it stayed broken.
    last_failed: bool,
}

impl ViewState {
    pub fn new() -> Self {
        Self {
            dirty: false,
            last_attempt: Instant::now(),
            last_failed: false,
        }
    }

    /// Note that the group's source rows changed. Cheap; does no IO.
    pub fn mark(&mut self) {
        self.dirty = true;
    }

    /// Whether this group should be rebuilt now.
    ///
    /// `caught_up` is false while the tick that just ran came back saturated,
    /// which is the signal that more rows are already queued behind it.
    pub fn due(&self, caught_up: bool) -> bool {
        self.dirty
            && ((caught_up && !self.last_failed)
                || self.last_attempt.elapsed() >= MIN_REFRESH_INTERVAL)
    }

    /// Record the outcome of an attempt.
    pub fn record(&mut self, ok: bool) {
        self.last_attempt = Instant::now();
        self.last_failed = !ok;
        // Cleared only on success: a failed refresh leaves the view stale, and
        // dropping the flag would mean nothing ever rebuilds it.
        if ok {
            self.dirty = false;
        }
    }
}

impl Default for ViewState {
    fn default() -> Self {
        Self::new()
    }
}

/// Log a refresh outcome and reduce it to whether it succeeded.
///
/// Failures are logged, never propagated: a stale dashboard view must not fail
/// the tick that consumed the events, which have already been written.
pub fn report<E: Display>(view: &str, result: Result<(), E>) -> bool {
    match result {
        Ok(()) => true,
        Err(e) => {
            warn!(view, error = %e, "materialized view refresh failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirty() -> ViewState {
        let mut st = ViewState::new();
        st.mark();
        st
    }

    /// A view nothing touched is never rebuilt, however often the tick asks.
    #[test]
    fn a_clean_view_is_never_due() {
        let st = ViewState::new();
        assert!(!st.due(true));
        assert!(!st.due(false));
    }

    /// Steady state: the first tick that is not saturated publishes the change,
    /// which is the same tick that used to do it inline.
    #[test]
    fn catching_up_publishes_immediately() {
        assert!(dirty().due(true));
    }

    /// The whole point: a saturated tick during backfill does not pay for a full
    /// aggregation.
    #[test]
    fn a_saturated_tick_waits_out_the_interval() {
        assert!(!dirty().due(false));
    }

    /// A backfill long enough to outlast the interval still publishes progress.
    #[test]
    fn a_long_backfill_still_publishes_on_the_interval() {
        let mut st = dirty();
        st.last_attempt = Instant::now() - MIN_REFRESH_INTERVAL;
        assert!(st.due(false));
    }

    /// A failure must not clear the flag, or the change is never published.
    #[test]
    fn a_failed_refresh_stays_dirty() {
        let mut st = dirty();
        st.record(false);
        assert!(st.dirty);
    }

    /// ...and must not spin: without the `last_failed` guard a broken view would
    /// retry on every caught-up tick.
    #[test]
    fn a_failed_refresh_backs_off_instead_of_retrying_every_tick() {
        let mut st = dirty();
        st.record(false);
        assert!(!st.due(true), "retried immediately after failing");

        st.last_attempt = Instant::now() - MIN_REFRESH_INTERVAL;
        assert!(st.due(true), "never retried after backing off");
    }

    #[test]
    fn a_successful_refresh_clears_the_flag() {
        let mut st = dirty();
        st.record(true);
        assert!(!st.dirty);
        assert!(!st.due(true));
    }
}
