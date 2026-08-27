//! Reusable tick driver for indexer-style services.
//!
//! Every indexer service repeats the same loop: enumerate chains, call
//! `tick_chain` on each, sleep, exit on shutdown. Implementing [`TickService`]
//! lets a service reuse that loop via [`run`].
//!
//! The driver is not a fixed-cadence timer. A tick reports what it accomplished
//! via [`TickProgress`] and the driver sleeps only when nothing is left to do,
//! so initial sync is bounded by the database rather than by `batch / tick_ms`.

use crate::backoff::Backoff;
use crate::shutdown::Shutdown;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, info, trace, warn};

/// A wake signal: input a tick service consumes may now be available.
///
/// Only the change carries meaning; the counter's value is arbitrary. The
/// driver holds it as an `Option`, where `None` is a poll-only service.
pub type Wake = watch::Receiver<u64>;

/// What one tick accomplished.
///
/// The ordering is significant: a round covering several chains takes the
/// maximum, so one chain with queued work keeps the driver off the sleep path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[must_use = "the driver sleeps unless a tick reports its progress"]
pub enum TickProgress {
    /// The cursor did not move: nothing queued, or blocked on work this batch
    /// cannot complete. Lets the idle delay grow.
    Idle,
    /// The cursor advanced but the batch was not filled: the queue is drained.
    /// Sleeps, but from the floor.
    Partial,
    /// The batch came back full, so more work is provably queued. Skips the
    /// idle delay entirely.
    Saturated,
}

impl TickProgress {
    /// Progress for a tick that advanced its cursor, given whether the batch
    /// came back full.
    ///
    /// A tick that moved nothing must report [`Idle`](Self::Idle) directly.
    pub fn advanced(batch_was_full: bool) -> Self {
        if batch_was_full {
            Self::Saturated
        } else {
            Self::Partial
        }
    }

    /// Stable label value for the `progress` metric dimension.
    ///
    /// Spelled out rather than derived from `Debug` so renaming a variant does
    /// not rename a time series that dashboards and alerts key on.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Partial => "partial",
            Self::Saturated => "saturated",
        }
    }

    /// [`advanced`](Self::advanced), deriving fullness from the row count.
    ///
    /// `>=` rather than `==` so a repository that over-reads its limit cannot
    /// downgrade a saturated batch to `Partial` and stall catch-up.
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
    /// Single tick for one chain. The driver logs and swallows errors so one
    /// failing chain does not stall the others.
    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<TickProgress>;
}

/// Tick every chain once, reporting the busiest outcome.
///
/// A failing chain is logged and contributes nothing, so it neither counts as
/// progress nor masks another chain's catch-up.
async fn run_round(svc: &dyn TickService, batch: i64) -> TickProgress {
    let mut round = TickProgress::Idle;
    for chain_id in svc.list_chain_ids().await {
        // Instrumented here rather than per service: this is the only place
        // that sees every tick with its name, chain and outcome. Binaries that
        // install no recorder emit nothing.
        let started = std::time::Instant::now();
        let outcome = svc.tick_chain(chain_id, batch).await;
        let service = svc.name();
        let chain = chain_id.to_string();

        metrics::histogram!(
            crate::metrics::name::TICK_DURATION,
            "service" => service,
            "chain_id" => chain.clone(),
        )
        .record(started.elapsed().as_secs_f64());

        match outcome {
            Ok(progress) => {
                metrics::counter!(
                    crate::metrics::name::TICK_PROGRESS,
                    "service" => service,
                    "chain_id" => chain,
                    "progress" => progress.label(),
                )
                .increment(1);
                round = round.max(progress);
            }
            Err(error) => {
                metrics::counter!(
                    crate::metrics::name::TICK_ERRORS,
                    "service" => service,
                    "chain_id" => chain,
                )
                .increment(1);
                warn!(name = service, chain_id, %error, "tick failed");
            }
        }
    }
    round
}

/// Drive a [`TickService`] until shutdown fires, polling only.
pub async fn run(svc: Arc<dyn TickService>, tick_ms: u64, batch: i64, shutdown: Shutdown) {
    run_with_wake(svc, tick_ms, batch, shutdown, None).await
}

/// Drive a [`TickService`] until shutdown fires.
///
/// `tick_ms` is the ceiling of the idle backoff, not a fixed period:
///
/// - [`Saturated`](TickProgress::Saturated): no sleep, so catch-up is bounded
///   by the database rather than by the tick.
/// - [`Partial`](TickProgress::Partial): sleep from the [`Backoff::idle`] floor,
///   so an arrival landing just after a round is picked up in ~50 ms.
/// - [`Idle`](TickProgress::Idle): sleep, doubling up to `tick_ms`.
///
/// `wake` cuts an idle sleep short when the producer signals new input; see
/// [`crate::backoff`] and the `database::listen` module. It supplements the
/// poll rather than replacing it: every consumer's cursor is durable, so a wake
/// that never arrives costs latency only. The `Saturated` path does not consult
/// it, since queued work is already known.
pub async fn run_with_wake(
    svc: Arc<dyn TickService>,
    tick_ms: u64,
    batch: i64,
    mut shutdown: Shutdown,
    mut wake: Option<Wake>,
) {
    let name = svc.name();
    let mut backoff = Backoff::idle(tick_ms);
    info!(name, tick_ms, batch, "tick driver started");

    // Checked before every round because the `Saturated` path below skips the
    // `select!`, leaving no other way to stop mid-catch-up.
    while !shutdown.is_triggered() {
        let round = run_round(svc.as_ref(), batch).await;

        if round > TickProgress::Idle {
            backoff.reset();
        }

        if round == TickProgress::Saturated {
            trace!(name, "work still queued; skipping the idle delay");
            // A yield, not a delay: this path awaits nothing else, so a service
            // whose ticks complete without suspending would never hand the
            // runtime back and would starve the signal handler.
            tokio::task::yield_now().await;
            continue;
        }

        let delay = backoff.next_delay();
        debug!(
            name,
            ?round,
            delay_ms = delay.as_millis(),
            "tick driver idle"
        );
        tokio::select! {
            _ = sleep(delay) => {}
            // Snap back to the floor: a wake means the producer is active, so
            // the escalated delay no longer matches the arrival rate.
            _ = woken(&mut wake) => {
                trace!(name, "woken; skipping the rest of the idle delay");
                backoff.reset();
            }
            _ = shutdown.recv() => break,
        }
    }

    info!(name, "tick driver stopping");
}

/// Resolve when `wake` fires; never, when there is none.
///
/// `pending()` rather than an `Option` guard on the `select!` arm so the other
/// arms are written once. A closed channel, meaning the listener task is gone,
/// also parks here forever: the poll still runs, and spinning on a dead sender
/// would become a busy wait.
async fn woken(wake: &mut Option<Wake>) {
    match wake {
        Some(rx) => {
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await
            }
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backoff::{IDLE_FACTOR as TICK_FACTOR, IDLE_MIN as TICK_MIN};
    use crate::shutdown;
    use std::sync::Mutex;
    use std::time::Duration;
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
        /// because these tests run on a paused clock, which auto-advances only
        /// while the runtime is idle; a spin loop would keep it awake and the
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
        assert_eq!(svc.gaps(), [TICK_MIN, TICK_MIN * TICK_FACTOR, TICK_MIN]);
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

    /// An arrival announced mid-sleep must be picked up immediately rather than
    /// after the escalated idle delay.
    #[tokio::test(start_paused = true)]
    async fn a_wake_cuts_an_idle_sleep_short() {
        let svc = Scripted::new([]); // always Idle, so the driver always sleeps
        let (waker, wake) = watch::channel(0u64);
        let (trigger, sd) = shutdown::channel();
        let mut ticks = svc.ticks.subscribe();
        let driver = tokio::spawn(run_with_wake(svc.clone(), CEILING_MS, 100, sd, Some(wake)));

        // Let the first tick run and the driver settle into its sleep.
        ticks.wait_for(|&n| n >= 1).await.expect("first tick");
        tokio::time::sleep(TICK_MIN / 2).await;
        waker.send_modify(|n| *n += 1);

        ticks
            .wait_for(|&n| n >= 2)
            .await
            .expect("wake did not tick");
        trigger.fire();
        driver.await.expect("driver panicked");

        let gap = svc.gaps()[0];
        assert_eq!(
            gap,
            TICK_MIN / 2,
            "expected the wake to end the sleep at once, slept {gap:?}"
        );
    }

    /// A wake resets the backoff, not just the current sleep. Otherwise a chain
    /// that escalated to the ceiling pays it again on the next gap even though
    /// the producer is demonstrably active.
    #[tokio::test(start_paused = true)]
    async fn a_wake_resets_the_backoff() {
        let svc = Scripted::new([]);
        let (waker, wake) = watch::channel(0u64);
        let (trigger, sd) = shutdown::channel();
        let mut ticks = svc.ticks.subscribe();
        let driver = tokio::spawn(run_with_wake(svc.clone(), CEILING_MS, 100, sd, Some(wake)));

        // Three idle ticks escalate the delay to TICK_MIN * 4.
        ticks.wait_for(|&n| n >= 3).await.expect("idle ticks");
        waker.send_modify(|n| *n += 1);
        ticks.wait_for(|&n| n >= 5).await.expect("post-wake ticks");
        trigger.fire();
        driver.await.expect("driver panicked");

        // gaps: [MIN, MIN*2, <wake>, MIN]. The gap after the wake is the
        // assertion; a driver that only skipped one sleep would show MIN*8.
        let gaps = svc.gaps();
        assert_eq!(
            gaps[3], TICK_MIN,
            "backoff was not reset by the wake: {gaps:?}"
        );
    }

    /// The `Saturated` path skips the `select!`, so a wake arriving during
    /// catch-up must change nothing and must not wedge the driver.
    #[tokio::test(start_paused = true)]
    async fn a_wake_during_saturated_changes_nothing() {
        let svc = Scripted::new([
            Ok(TickProgress::Saturated),
            Ok(TickProgress::Saturated),
            Ok(TickProgress::Saturated),
        ]);
        let (waker, wake) = watch::channel(0u64);
        waker.send_modify(|n| *n += 1); // already pending before the run starts
        let (trigger, sd) = shutdown::channel();
        let mut ticks = svc.ticks.subscribe();
        let driver = tokio::spawn(run_with_wake(svc.clone(), CEILING_MS, 100, sd, Some(wake)));
        ticks.wait_for(|&n| n >= 3).await.expect("saturated ticks");
        trigger.fire();
        driver.await.expect("driver panicked");

        assert!(
            svc.gaps().iter().all(|g| g.is_zero()),
            "slept between saturated ticks: {:?}",
            svc.gaps()
        );
    }

    /// A dropped sender means the listener task is gone. The poll must continue
    /// at its normal cadence rather than spin on a channel that cannot fire.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_waker_falls_back_to_the_poll() {
        let svc = Scripted::new([]);
        let (waker, wake) = watch::channel(0u64);
        drop(waker);
        let (trigger, sd) = shutdown::channel();
        let mut ticks = svc.ticks.subscribe();
        let driver = tokio::spawn(run_with_wake(svc.clone(), CEILING_MS, 100, sd, Some(wake)));
        ticks.wait_for(|&n| n >= 4).await.expect("polled ticks");
        trigger.fire();
        driver.await.expect("driver panicked");

        assert_eq!(
            svc.gaps(),
            [
                TICK_MIN,
                TICK_MIN * TICK_FACTOR,
                TICK_MIN * TICK_FACTOR * TICK_FACTOR
            ],
            "a dead waker must leave the idle schedule untouched"
        );
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
