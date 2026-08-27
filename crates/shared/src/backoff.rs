//! Exponential backoff for idle polling loops.
//!
//! Used by the [tick driver](crate::tick) to keep a drained service cheap
//! without paying a fixed tick of latency on the next arrival: the delay starts
//! near zero and grows only while nothing is happening.

use std::time::Duration;

/// Starts at `initial`, multiplies by `factor` each step, capped at `max`.
///
/// Call [`next_delay`](Self::next_delay) when idle to get the current sleep
/// and advance; call [`reset`](Self::reset) after useful work to snap back to
/// `initial`.
pub struct Backoff {
    current: Duration,
    initial: Duration,
    max: Duration,
    factor: u32,
}

/// Floor of an idle poll's delay: how soon after going quiet a driver looks
/// again. Small enough that an arrival landing just after a poll is not made to
/// wait for the ceiling.
///
/// Public so a driver's tests can assert the ladder they expect without
/// restating it, which is how the two would drift.
pub const IDLE_MIN: Duration = Duration::from_millis(50);

/// Growth factor of an idle poll's delay.
pub const IDLE_FACTOR: u32 = 2;

impl Backoff {
    /// The workspace's idle-polling backoff: floor at [`IDLE_MIN`], doubling up
    /// to `ceiling_ms`.
    ///
    /// Every polling loop wants the same three decisions — the floor, the
    /// growth factor, and what to do about a misconfigured zero — so they live
    /// here rather than being restated at each driver. Lowering the floor is
    /// then one edit, not one per caller.
    ///
    /// `max(1)` keeps a zero ceiling from producing a zero delay, which
    /// [`Backoff::new`] rejects and which would busy-poll. The floor is clamped
    /// to the ceiling so a sub-floor ceiling is honoured rather than rounded up.
    pub fn idle(ceiling_ms: u64) -> Self {
        let ceiling = Duration::from_millis(ceiling_ms.max(1));
        Self::new(IDLE_MIN.min(ceiling), ceiling, IDLE_FACTOR)
    }

    /// # Panics
    ///
    /// If `initial` is zero, exceeds `max`, or `factor < 2`. These are hard
    /// asserts rather than `debug_assert!`s because each case yields a delay
    /// that never grows, making a polling caller spin at full speed in release.
    pub fn new(initial: Duration, max: Duration, factor: u32) -> Self {
        assert!(!initial.is_zero(), "initial delay must be non-zero");
        assert!(initial <= max, "initial delay must not exceed max");
        assert!(factor >= 2, "factor must be at least 2");
        Self {
            current: initial,
            initial,
            max,
            factor,
        }
    }

    /// The current delay; advances to the next step.
    #[must_use = "the delay must be awaited, or the backoff has no effect"]
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = self.current.saturating_mul(self.factor).min(self.max);
        delay
    }

    /// The delay the next call to [`next_delay`](Self::next_delay) will
    /// return, without advancing. For logging and assertions.
    #[must_use]
    pub fn peek(&self) -> Duration {
        self.current
    }

    /// Snap back to `initial`. Call after doing useful work.
    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn doubles_and_caps_at_max() {
        let mut b = Backoff::new(ms(50), ms(5000), 2);
        let seen: Vec<_> = (0..9).map(|_| b.next_delay()).collect();
        assert_eq!(
            seen,
            [
                ms(50),
                ms(100),
                ms(200),
                ms(400),
                ms(800),
                ms(1600),
                ms(3200),
                ms(5000),
                ms(5000)
            ]
        );
    }

    #[test]
    fn reset_snaps_to_initial() {
        let mut b = Backoff::new(ms(50), ms(5000), 2);
        for _ in 0..3 {
            let _ = b.next_delay();
        }
        b.reset();
        assert_eq!(b.next_delay(), ms(50));
    }

    #[test]
    fn honours_a_non_binary_factor() {
        let mut b = Backoff::new(ms(10), ms(1000), 3);
        let seen: Vec<_> = (0..6).map(|_| b.next_delay()).collect();
        assert_eq!(seen, [ms(10), ms(30), ms(90), ms(270), ms(810), ms(1000)]);
    }

    #[test]
    fn peek_reports_the_next_delay_without_advancing() {
        let mut b = Backoff::new(ms(50), ms(5000), 2);
        assert_eq!(b.peek(), ms(50));
        assert_eq!(b.peek(), ms(50), "peek must not advance");
        let _ = b.next_delay();
        assert_eq!(b.peek(), ms(100));
    }

    #[test]
    #[should_panic(expected = "initial delay must be non-zero")]
    fn a_zero_initial_is_rejected() {
        // Would stay zero forever: saturating_mul(0, n) == 0.
        Backoff::new(Duration::ZERO, ms(5000), 2);
    }

    #[test]
    #[should_panic(expected = "factor must be at least 2")]
    fn a_factor_below_two_is_rejected() {
        // Would never grow, busy-looping at `initial`.
        Backoff::new(ms(50), ms(5000), 1);
    }

    #[test]
    #[should_panic(expected = "initial delay must not exceed max")]
    fn an_initial_above_max_is_rejected() {
        Backoff::new(ms(5000), ms(50), 2);
    }

    #[test]
    fn an_initial_equal_to_max_never_grows() {
        let mut b = Backoff::new(ms(500), ms(500), 2);
        assert_eq!(b.next_delay(), ms(500));
        assert_eq!(b.next_delay(), ms(500));
    }

    /// The floor and the zero-guard are the whole point of [`Backoff::idle`];
    /// a caller that reimplemented them would drift silently.
    #[test]
    fn idle_starts_at_the_floor_and_climbs_to_the_ceiling() {
        let mut b = Backoff::idle(2_000);
        assert_eq!(b.next_delay(), ms(50), "first look is soon");
        assert_eq!(b.next_delay(), ms(100));
        for _ in 0..10 {
            let _ = b.next_delay();
        }
        assert_eq!(b.peek(), ms(2_000), "caps at the ceiling");
    }

    /// A ceiling below the floor must be honoured, not rounded up to it.
    #[test]
    fn idle_honours_a_ceiling_below_the_floor() {
        let mut b = Backoff::idle(10);
        assert_eq!(b.next_delay(), ms(10));
    }

    /// `Backoff::new` panics on a zero initial delay, so the guard has to live
    /// in the constructor rather than at each call site.
    #[test]
    fn idle_survives_a_zero_ceiling() {
        assert_eq!(Backoff::idle(0).peek(), ms(1));
    }
}
