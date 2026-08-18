//! Reusable tick driver for indexer-style services.
//!
//! Every indexer service repeats: enumerate chains, call `tick_chain` on
//! each, sleep, exit on shutdown. Implementing [`TickService`] lets a service
//! reuse the loop via [`run`] without rewriting the boilerplate.
//!
//! The driver is *not* a fixed-cadence timer. A tick reports what it
//! accomplished via [`TickProgress`], and the driver sleeps only when there is
//! nothing left to do — otherwise initial sync would be pinned at
//! `batch / tick_ms` regardless of how fast the database could go.

use crate::backoff::Backoff;
use crate::shutdown::Shutdown;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, trace, warn};

/// Floor of the idle backoff. The configured `tick_ms` is the ceiling.
const TICK_MIN: Duration = Duration::from_millis(50);
/// Growth factor of the idle backoff.
const TICK_FACTOR: u32 = 2;

/// What one tick accomplished.
///
/// The ordering is load-bearing: a round covering several chains takes the
/// maximum, so one chain still holding queued work keeps the whole driver off
/// the sleep path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[must_use = "the driver sleeps unless a tick reports its progress"]
pub enum TickProgress {
    /// The cursor did not move — nothing queued, or blocked on work this
    /// batch cannot complete. Lets the idle delay grow.
    Idle,
    /// The cursor advanced but the batch was not filled: the queue is drained.
    /// Sleeps, but from the floor.
    Partial,
    /// The batch came back full, so more work is provably queued. Skips the
    /// idle delay entirely.
    Saturated,
}

impl TickProgress {
    /// Progress for a tick that **advanced its cursor**, given whether the
    /// batch came back full.
    ///
    /// A tick that moved nothing is [`Idle`](Self::Idle) and should say so
    /// directly — "full batch" is meaningless there.
    pub fn advanced(batch_was_full: bool) -> Self {
        if batch_was_full {
            Self::Saturated
        } else {
            Self::Partial
        }
    }

    /// [`advanced`](Self::advanced), deriving fullness from the row count.
    ///
    /// `>=` rather than `==` so a repository that over-reads its limit cannot
    /// silently downgrade a saturated batch to `Partial` and stall catch-up.
    pub fn from_batch(rows: usize, batch: i64) -> Self {
        Self::advanced(batch > 0 && rows as u64 >= batch as u64)
    }
}

#[async_trait]
pub trait TickService: Send + Sync {
    /// Human-readable name for logs.
    fn name(&self) -> &'static str;
    /// Chains the service should tick on this round.
    async fn list_chain_ids(&self) -> Vec<i64>;
    /// Single tick for one chain. Errors are logged + swallowed by the
    /// driver so one chain failing doesn't stall the others.
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<TickProgress>;
}

/// Tick every chain once, reporting the busiest outcome.
///
/// A failing chain is logged and contributes nothing: it must not masquerade
/// as progress and spin the loop, nor mask another chain's catch-up.
async fn run_round(svc: &dyn TickService, batch: i64) -> TickProgress {
    let mut round = TickProgress::Idle;
    for chain_id in svc.list_chain_ids().await {
        match svc.tick_chain(chain_id, batch).await {
            Ok(progress) => round = round.max(progress),
            Err(error) => warn!(name = svc.name(), chain_id, %error, "tick failed"),
        }
    }
    round
}

/// Drive a [`TickService`] until shutdown fires.
///
/// `tick_ms` is the **ceiling** of the idle backoff, not a fixed period:
///
/// - [`Saturated`](TickProgress::Saturated) — no sleep at all, so catch-up is
///   bounded by the database rather than by the tick.
/// - [`Partial`](TickProgress::Partial) — sleep, but from [`TICK_MIN`], so an
///   arrival landing just after a round is picked up in ~50 ms.
/// - [`Idle`](TickProgress::Idle) — sleep, doubling up to `tick_ms`.
pub async fn run(svc: Arc<dyn TickService>, tick_ms: u64, batch: i64, mut shutdown: Shutdown) {
    let name = svc.name();
    // `max(1)` keeps a misconfigured `tick_ms = 0` from producing a zero
    // backoff, which `Backoff::new` rejects and which would busy-poll anyway.
    let ceiling = Duration::from_millis(tick_ms.max(1));
    let mut backoff = Backoff::new(TICK_MIN.min(ceiling), ceiling, TICK_FACTOR);
    info!(name, tick_ms, batch, "tick driver started");

    // Checked before every round because the `Saturated` path below skips the
    // `select!`; without it the process would be unkillable mid-catch-up.
    while !shutdown.is_triggered() {
        let round = run_round(svc.as_ref(), batch).await;

        if round > TickProgress::Idle {
            backoff.reset();
        }

        if round == TickProgress::Saturated {
            trace!(name, "work still queued; skipping the idle delay");
            // A yield, not a delay. This path never awaits anything else, so a
            // service whose ticks complete without suspending would never hand
            // the runtime back — starving the signal handler.
            tokio::task::yield_now().await;
            continue;
        }

        let delay = backoff.next_delay();
        debug!(name, ?round, delay_ms = delay.as_millis(), "tick driver idle");
        tokio::select! {
            _ = sleep(delay) => {}
            _ = shutdown.recv() => break,
        }
    }

    info!(name, "tick driver stopping");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shutdown;
    use std::sync::Mutex;
    use tokio::sync::watch;
    use tokio::time::Instant;

    const CEILING_MS: u64 = 10_000;

    /// Replays a scripted sequence of outcomes, recording when each tick ran.
    ///
    /// The script is consumed front-to-back; once exhausted every further tick
    /// reports `Idle`.
    struct Scripted {
        script: Mutex<std::collections::VecDeque<anyhow::Result<TickProgress>>>,
        ticked_at: Mutex<Vec<Instant>>,
        /// Published tick count. A watch channel rather than a polled counter
        /// because these tests run on a paused clock, which only auto-advances
        /// while the runtime is idle — a spin loop would pin it awake and the
        /// driver's `sleep` would never resolve.
        ticks: watch::Sender<usize>,
    }

    impl Scripted {
        fn new(script: impl IntoIterator<Item = anyhow::Result<TickProgress>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into_iter().collect()),
                ticked_at: Mutex::new(Vec::new()),
                ticks: watch::channel(0).0,
            })
        }

        /// Virtual time between consecutive ticks. Exact, because these tests
        /// run on a paused clock.
        fn gaps(&self) -> Vec<Duration> {
            let at = self.ticked_at.lock().unwrap();
            at.windows(2).map(|w| w[1] - w[0]).collect()
        }
    }

    #[async_trait]
    impl TickService for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn list_chain_ids(&self) -> Vec<i64> {
            vec![1]
        }
        async fn tick_chain(&self, _chain_id: i64, _batch: i64) -> anyhow::Result<TickProgress> {
            let count = {
                let mut at = self.ticked_at.lock().unwrap();
                at.push(Instant::now());
                at.len()
            };
            self.ticks.send_replace(count);
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(TickProgress::Idle))
        }
    }

    /// Drive `svc` until it has ticked `n` times, then shut the driver down.
    ///
    /// Awaits the watch channel rather than polling, so the runtime goes idle
    /// between ticks and the paused clock auto-advances over each sleep.
    async fn drive_until(svc: Arc<Scripted>, n: usize) {
        let (trigger, sd) = shutdown::channel();
        let mut ticks = svc.ticks.subscribe();
        let driver = tokio::spawn(run(svc.clone(), CEILING_MS, 100, sd));
        ticks
            .wait_for(|&count| count >= n)
            .await
            .expect("tick counter dropped");
        trigger.fire();
        driver.await.expect("driver panicked");
    }

    #[tokio::test(start_paused = true)]
    async fn saturated_rounds_never_sleep() {
        let svc = Scripted::new([
            Ok(TickProgress::Saturated),
            Ok(TickProgress::Saturated),
            Ok(TickProgress::Saturated),
        ]);
        drive_until(svc.clone(), 3).await;
        assert!(
            svc.gaps().iter().all(|g| g.is_zero()),
            "slept between saturated ticks: {:?}",
            svc.gaps()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_rounds_escalate_to_the_ceiling() {
        let svc = Scripted::new([]); // exhausted script => always Idle
        drive_until(svc.clone(), 4).await;
        assert_eq!(
            svc.gaps(),
            [
                TICK_MIN,
                TICK_MIN * TICK_FACTOR,
                TICK_MIN * TICK_FACTOR * TICK_FACTOR
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn partial_sleeps_but_resets_the_floor() {
        let svc = Scripted::new([
            Ok(TickProgress::Idle),
            Ok(TickProgress::Idle),
            Ok(TickProgress::Partial),
        ]);
        drive_until(svc.clone(), 4).await;
        // Two idles escalate; the Partial must drop back to the floor.
        assert_eq!(
            svc.gaps(),
            [TICK_MIN, TICK_MIN * TICK_FACTOR, TICK_MIN]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_chain_does_not_count_as_progress() {
        let svc = Scripted::new([
            Err(anyhow::anyhow!("boom")),
            Err(anyhow::anyhow!("boom")),
            Err(anyhow::anyhow!("boom")),
        ]);
        drive_until(svc.clone(), 3).await;
        // Identical to the all-idle schedule: errors must not spin the loop.
        assert_eq!(svc.gaps(), [TICK_MIN, TICK_MIN * TICK_FACTOR]);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_interrupts_a_saturated_catch_up() {
        // Always saturated, so the driver never reaches the `select!`; only the
        // top-of-loop check can stop it.
        struct Always;
        #[async_trait]
        impl TickService for Always {
            fn name(&self) -> &'static str {
                "always"
            }
            async fn list_chain_ids(&self) -> Vec<i64> {
                vec![1]
            }
            async fn tick_chain(&self, _: i64, _: i64) -> anyhow::Result<TickProgress> {
                Ok(TickProgress::Saturated)
            }
        }
        let (trigger, sd) = shutdown::channel();
        let driver = tokio::spawn(run(Arc::new(Always), CEILING_MS, 100, sd));
        tokio::task::yield_now().await;
        trigger.fire();
        tokio::time::timeout(Duration::from_secs(5), driver)
            .await
            .expect("driver did not observe shutdown during catch-up")
            .expect("driver panicked");
    }

    #[tokio::test(start_paused = true)]
    async fn a_round_reports_the_busiest_chain() {
        // One chain saturated among idle ones must keep the driver off the
        // sleep path — otherwise a quiet chain throttles a busy one.
        struct PerChain;
        #[async_trait]
        impl TickService for PerChain {
            fn name(&self) -> &'static str {
                "per-chain"
            }
            async fn list_chain_ids(&self) -> Vec<i64> {
                vec![1, 2, 3]
            }
            async fn tick_chain(&self, chain_id: i64, _: i64) -> anyhow::Result<TickProgress> {
                Ok(if chain_id == 2 {
                    TickProgress::Saturated
                } else {
                    TickProgress::Idle
                })
            }
        }
        assert_eq!(run_round(&PerChain, 100).await, TickProgress::Saturated);
    }

    #[test]
    fn progress_orders_idle_below_partial_below_saturated() {
        assert!(TickProgress::Idle < TickProgress::Partial);
        assert!(TickProgress::Partial < TickProgress::Saturated);
    }

    #[test]
    fn from_batch_saturates_only_on_a_full_batch() {
        assert_eq!(TickProgress::from_batch(99, 100), TickProgress::Partial);
        assert_eq!(TickProgress::from_batch(100, 100), TickProgress::Saturated);
        // An over-read must not read as "drained".
        assert_eq!(TickProgress::from_batch(101, 100), TickProgress::Saturated);
        // A nonsensical batch size must not claim saturation forever.
        assert_eq!(TickProgress::from_batch(0, 0), TickProgress::Partial);
    }
}
