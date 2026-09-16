//! The backfill's view of the `notes.id` head, held back until it is safe to
//! page over.

use std::time::{Duration, Instant};

/// How long a `notes.id` must have been observable before the backfill pages
/// past it.
///
/// `notes.id` comes from a sequence, so it is allocated before commit and ids do
/// not become visible in id order. That is harmless for the per-chain forward
/// pass, where the consume lock gives each chain one writer, but the backfill
/// walks a single global pointer and two replicas leading two chains interleave.
/// Reading the head from `max(id)` would step over a row that commits a moment
/// later, and the pointer never goes back, leaving that note unscanned for the
/// subscription.
///
/// Lagging the head bounds the hazard by how long a single `INSERT` can stay
/// uncommitted rather than by id ordering: five seconds against a sub-second
/// statement.
pub(super) const BACKFILL_LAG: Duration = Duration::from_secs(5);

/// A `notes.id` head held back by [`BACKFILL_LAG`].
///
/// Keeps two observations: `safe` is old enough to page over and `pending` is
/// waiting out the lag. The clock resets only on promotion, so calling this
/// several times per tick, as the per-chain backfill does, does not push the
/// promotion further out.
pub(super) struct LaggedHead {
    safe: i64,
    pending: i64,
    pending_since: Instant,
}

impl LaggedHead {
    pub(super) fn new() -> Self {
        Self {
            safe: 0,
            pending: 0,
            pending_since: Instant::now(),
        }
    }

    pub(super) fn observe(&mut self, max_id: i64) -> i64 {
        if self.pending_since.elapsed() >= BACKFILL_LAG {
            self.safe = self.pending;
            self.pending = max_id;
            self.pending_since = Instant::now();
        }
        self.safe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lagged_head_withholds_ids_until_they_have_aged() {
        let mut head = LaggedHead::new();

        // Ids written this instant are not offered: a peer's uncommitted row
        // could still be interleaved below them.
        assert_eq!(head.observe(100), 0);
        assert_eq!(head.observe(150), 0, "and the clock is not reset per call");

        head.pending_since -= BACKFILL_LAG;
        assert_eq!(head.observe(200), 0, "promotes the first observation");
        head.pending_since -= BACKFILL_LAG;
        assert_eq!(head.observe(250), 200, "which is now old enough to page");
    }
}
