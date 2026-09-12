//! Adaptive `eth_getLogs` windowing.
//!
//! Providers cap how much one query may return, by block span, by response size
//! or both, and disagree on the limit and on how they report reaching it. Rather
//! than a per-provider constant, the cap is learned: narrow on rejection, and
//! once enough full-size windows have been served, probe back upward — the only
//! evidence available is the provider's own error wording, so the belief has to
//! be able to recover from misreading it.
//!
//! Once the cap is known the range splits into windows whose sizes need no
//! feedback from each other, so they are fetched concurrently under a budget
//! shared by every caller on the chain. Only the search itself is serial.

use crate::adapters::DynRpc;
use crate::domain::error::{IngesterError, RpcError};
use crate::domain::models::RawEvent;
use crate::services::decode::{distinct_blocks, logs_to_rows};
use alloy::primitives::Address;
use alloy::rpc::types::eth::Log;
use futures::stream::{self, StreamExt, TryStreamExt};
use shared::metrics::{ingest_stage, timed_ingest_stage};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::debug;

/// Full-size windows the provider must serve before the learned cap is probed
/// upward.
///
/// The cap is inferred from provider error strings, so a string that is misread
/// pins every later query below what the provider actually serves. A cap that
/// can only fall makes that permanent for the life of the process; probing makes
/// it self-healing. At this threshold the probe costs under one percent of
/// requests, and a wrongly-lowered cap climbs back out within a few hundred
/// windows.
const CONFIRMATIONS_BEFORE_PROBE: u64 = 128;

/// How large an `eth_getLogs` this provider will serve, as currently believed.
///
/// A property of the provider and its plan rather than of one query, so it is
/// learned once and shared: re-probing per call means every backfill chunk
/// repeats the same halving search, and the usual rejection is a timeout, where
/// the provider burns `rpc_timeout_ms` before refusing.
///
/// The three fields are one belief, which is why they are grouped rather than
/// left loose on [`LogWindow`]: `limit` moves only in [`Self::refuse`] and
/// [`Self::confirm`], and each of those keeps `served` and `confirmations`
/// consistent with the move.
#[derive(Debug)]
struct LearnedCap {
    /// Largest span currently believed servable. `u64::MAX` until the provider
    /// first refuses something.
    limit: AtomicU64,
    /// Largest span the provider has actually served. Evidence, and the floor an
    /// overshooting probe falls back to.
    served: AtomicU64,
    /// Windows served at the full `limit` since it last moved. Only full-size
    /// windows count: a short tail, or a live tick scanning three blocks, says
    /// nothing about where the cap is.
    confirmations: AtomicU64,
}

impl LearnedCap {
    fn new() -> Self {
        Self {
            limit: AtomicU64::new(u64::MAX),
            served: AtomicU64::new(0),
            confirmations: AtomicU64::new(0),
        }
    }

    fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Record that `rejected` was refused, and answer with the span to retry at.
    ///
    /// Never raises `limit`: a size that once failed must not be retried because
    /// another chunk happened to succeed at it.
    fn refuse(&self, rejected: u64) -> u64 {
        let served = self.served.load(Ordering::Relaxed);
        let halved = (rejected / 2).max(1);
        let next = if rejected > served {
            // Above a span the provider has already served, so this is a probe
            // overshooting rather than the provider tightening. Falling back to
            // the known-good span rather than to half the probe is what makes
            // probing free: halving from the probe would land *below* where the
            // limit started, so every probe would cost more than it could win.
            halved.max(served)
        } else {
            // At or below a span that used to work, so the provider itself has
            // changed and the old high-water mark is no longer evidence.
            self.served.fetch_min(halved, Ordering::Relaxed);
            halved
        };
        self.limit.fetch_min(next, Ordering::Relaxed);
        self.confirmations.store(0, Ordering::Relaxed);
        next
    }

    /// Record one window of `span` served without complaint, and once
    /// [`CONFIRMATIONS_BEFORE_PROBE`] full-size ones have been, probe upward.
    fn confirm(&self, span: u64) {
        self.served.fetch_max(span, Ordering::Relaxed);
        let limit = self.limit();
        // A window short of the limit is no evidence about where the limit is.
        // This also covers the unbounded case for free: nothing is shorter than
        // `u64::MAX`, and an unbounded limit has nothing to recover to anyway.
        if span < limit {
            return;
        }
        if self.confirmations.fetch_add(1, Ordering::Relaxed) + 1 < CONFIRMATIONS_BEFORE_PROBE {
            return;
        }
        self.confirmations.store(0, Ordering::Relaxed);
        // The `+1` floor matters: in integer arithmetic a limit of 1 grows by
        // half to 1, and 1 is exactly where a burst of misread errors leaves it.
        let raised = limit.saturating_add((limit / 2).max(1));
        // Compare-exchange rather than a store, so a concurrent `refuse` that
        // learned something real is not undone by a probe from a stale read. A
        // lost race needs another full round of confirmations, which is the
        // conservative direction.
        let _ = self
            .limit
            .compare_exchange(limit, raised, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// This chain's shared `eth_getLogs` policy: how large a query may be, and how
/// many may be in flight.
///
/// Both belong to one object per chain because both describe the provider rather
/// than the call. Holding the concurrency limit here is the same argument
/// [`crate::adapters::rpc::HttpRpc`] makes for its block-metadata semaphore: a
/// bound applied inside one fetch is silently multiplied by the number of
/// concurrent backfill chunks, so it is not the bound the config promises.
#[derive(Debug)]
pub struct LogWindow {
    cap: LearnedCap,
    permits: Semaphore,
    concurrency: usize,
}

impl LogWindow {
    pub fn new(concurrency: usize) -> Self {
        let concurrency = concurrency.max(1);
        Self {
            cap: LearnedCap::new(),
            permits: Semaphore::new(concurrency),
            concurrency,
        }
    }

    /// Wait for the right to issue one `eth_getLogs` against this provider.
    async fn permit(&self) -> SemaphorePermit<'_> {
        // `acquire` fails only on a closed semaphore, and nothing closes this
        // one; it lives as long as the `Arc` every caller holds. Surfacing the
        // impossible case as an RPC error would put a fault of ours in the
        // provider's error counters.
        self.permits
            .acquire()
            .await
            .expect("log window semaphore is never closed")
    }
}

/// The window size search.
///
/// Split out from the fetch loop so the sizing rule is testable without a
/// provider. Holds only the current size: the belief itself lives in
/// [`LearnedCap`], so there is one authoritative copy and a shrink cannot learn
/// something the next call forgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    size: u64,
}

impl Window {
    /// Start at whatever the span asks for, capped by what the provider is
    /// already known to refuse.
    fn new(span: u64, limit: u64) -> Self {
        Self {
            size: span.min(limit).max(1),
        }
    }

    /// Whether the window can shrink further, or a single block is already too
    /// much.
    fn can_shrink(&self) -> bool {
        self.size > 1
    }

    /// Narrow, and publish the new size as a span the provider has refused
    /// above.
    ///
    /// Publishing is what stops the window doubling straight back into the cap
    /// after every shrink, which would make half of all requests fail — and what
    /// lets a sibling chunk skip the search entirely. How far to narrow is
    /// [`LearnedCap::refuse`]'s decision, since only it knows what has already
    /// been served.
    fn shrink(&mut self, cap: &LearnedCap) {
        self.size = cap.refuse(self.size);
    }

    fn grow(&mut self, remaining: u64, limit: u64) {
        self.size = self.size.saturating_mul(2).min(remaining).min(limit).max(1);
    }
}

/// Fetch every matching log in `[from, to]`, narrowing the query window to
/// whatever the provider will actually serve.
///
/// Anything that is not a range cap propagates: rate limits and transport errors
/// belong to the retry layer.
///
/// `learned` carries the provider's cap between calls, so only the first fetch
/// against a provider pays the halving search.
pub async fn fetch_adaptive(
    rpc: &DynRpc,
    learned: &LogWindow,
    address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<Log>, IngesterError> {
    let span = to.saturating_sub(from).saturating_add(1);
    let limit = learned.cap.limit();

    // A cap that is already known, and smaller than what was asked for, makes the
    // split arithmetic deterministic: none of the sub-window sizes depend on a
    // previous response, so they can go out together instead of the range being
    // walked one round trip at a time. Only the search itself needs feedback.
    if limit < span {
        match fetch_split(rpc, learned, address, from, to, limit).await {
            Ok(logs) => return Ok(logs),
            // The cap moved under us, or a probe overshot it. Fall through: the
            // serial walk re-reads the limit and re-learns it. Worth a line,
            // because the fallback re-fetches windows that had already landed
            // and an operator watching throughput deserves the reason.
            Err(IngesterError::Rpc(RpcError::RangeTooLarge)) => {
                debug!(
                    from,
                    to, limit, "log window rejected at the learned cap; re-probing"
                );
            }
            Err(e) => return Err(e),
        }
    }

    fetch_probing(rpc, learned, address, from, to).await
}

/// Fetch `[from, to]` as concurrent windows of `size`, which the provider is
/// already believed to accept.
///
/// Fails as a whole on a range rejection, leaving the caller to fall back: a
/// partial result cannot be stitched to a re-walk without tracking which windows
/// landed, and the case only arises when the learned cap is stale.
async fn fetch_split(
    rpc: &DynRpc,
    learned: &LogWindow,
    address: Address,
    from: u64,
    to: u64,
    size: u64,
) -> Result<Vec<Log>, IngesterError> {
    // `refuse` floors at one, so this only guards a caller passing its own size.
    let size = size.max(1);
    let starts = std::iter::successors(Some(from), move |&start| {
        let next = start.saturating_add(size);
        (next <= to).then_some(next)
    });

    stream::iter(starts.map(|start| {
        let end = start.saturating_add(size - 1).min(to);
        async move {
            let _permit = learned.permit().await;
            let logs = rpc.fetch_logs(address, start, end).await?;
            learned.cap.confirm(end - start + 1);
            Ok::<_, IngesterError>(logs)
        }
    }))
    // Ordered rather than unordered: the concurrency is already capped by the
    // provider's semaphore, so yielding in submission order costs nothing and
    // keeps a replayed range decoding identically.
    .buffered(learned.concurrency)
    // Folded rather than collected: a chunk spanning a busy contract holds every
    // window's logs at once either way, but collecting into a `Vec<Vec<Log>>` and
    // concatenating holds them twice.
    .try_fold(Vec::new(), |mut acc, logs| async move {
        absorb(&mut acc, logs);
        Ok(acc)
    })
    .await
}

/// Move one window's logs into the accumulator.
///
/// Taking the first response whole keeps the common case — a single window
/// covering the whole range — free of a copy.
fn absorb(acc: &mut Vec<Log>, mut logs: Vec<Log>) {
    if acc.is_empty() {
        *acc = logs;
    } else {
        acc.append(&mut logs);
    }
}

/// Walk `[from, to]` one window at a time, halving on rejection.
///
/// The path that learns the cap. Serial by necessity: each size is chosen from
/// the previous response.
async fn fetch_probing(
    rpc: &DynRpc,
    learned: &LogWindow,
    address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<Log>, IngesterError> {
    let span = to.saturating_sub(from).saturating_add(1);
    let mut window = Window::new(span, learned.cap.limit());
    let mut cursor = from;
    let mut acc = Vec::new();

    while cursor <= to {
        let end = cursor.saturating_add(window.size - 1).min(to);
        let result = {
            let _permit = learned.permit().await;
            rpc.fetch_logs(address, cursor, end).await
        };
        match result {
            Ok(logs) => {
                learned.cap.confirm(end - cursor + 1);
                absorb(&mut acc, logs);
                cursor = end + 1;
                // Re-read the limit rather than reusing the one sampled at
                // entry: a sibling chunk may have found the cap while this one
                // was in flight, and the first backfill wave would otherwise pay
                // the search once per concurrent chunk.
                window.grow(
                    to.saturating_sub(cursor).saturating_add(1),
                    learned.cap.limit(),
                );
            }
            Err(IngesterError::Rpc(RpcError::RangeTooLarge)) if window.can_shrink() => {
                window.shrink(&learned.cap);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(acc)
}

/// Fetch `[from, to]` and decode it into insertable rows.
///
/// The whole read side of a scan, shared by the live tick and the backfill so
/// the two cannot drift on stage labels or on how block metadata is resolved.
/// The caller decides what an empty result means: the live tick only advances
/// its watermark, while the backfill still commits the chunk.
pub async fn fetch_rows(
    rpc: &DynRpc,
    learned: &LogWindow,
    chain_id: i64,
    address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<RawEvent>, IngesterError> {
    let logs = timed_ingest_stage(
        ingest_stage::GET_LOGS,
        chain_id,
        fetch_adaptive(rpc, learned, address, from, to),
    )
    .await?;
    if logs.is_empty() {
        return Ok(Vec::new());
    }
    let block_meta = timed_ingest_stage(
        ingest_stage::BLOCK_META,
        chain_id,
        rpc.fetch_block_meta(&distinct_blocks(&logs)),
    )
    .await?;
    logs_to_rows(chain_id, logs, &block_meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::rpc::{BlockMeta, ChainRpc};
    use alloy::primitives::B256;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Serves one log per block, but only for windows up to `max_span`.
    struct CappedRpc {
        max_span: u64,
        calls: AtomicUsize,
        rejections: AtomicUsize,
    }

    impl CappedRpc {
        fn new(max_span: u64) -> Arc<Self> {
            Arc::new(Self {
                max_span,
                calls: AtomicUsize::new(0),
                rejections: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl ChainRpc for CappedRpc {
        async fn tip(&self) -> Result<u64, IngesterError> {
            unimplemented!("not exercised")
        }
        async fn fetch_logs(
            &self,
            _address: Address,
            from: u64,
            to: u64,
        ) -> Result<Vec<Log>, IngesterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if to - from + 1 > self.max_span {
                self.rejections.fetch_add(1, Ordering::SeqCst);
                return Err(IngesterError::Rpc(RpcError::RangeTooLarge));
            }
            Ok((from..=to).map(|_| Log::default()).collect())
        }
        async fn fetch_block_meta(
            &self,
            _blocks: &[u64],
        ) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
            unimplemented!("not exercised")
        }
        async fn block_hash_at(&self, _n: u64) -> Result<Option<B256>, IngesterError> {
            unimplemented!("not exercised")
        }
    }

    #[tokio::test]
    async fn narrows_until_the_provider_accepts_and_covers_the_whole_range() {
        let rpc = CappedRpc::new(10);
        let logs = fetch_adaptive(
            &(rpc.clone() as DynRpc),
            &LogWindow::new(8),
            Address::ZERO,
            0,
            99,
        )
        .await
        .expect("range cap is recoverable");
        assert_eq!(logs.len(), 100, "every block covered exactly once");
    }

    /// Without a learned cap the window doubles straight back into it
    /// after every success, so roughly half of all requests fail.
    #[tokio::test]
    async fn does_not_climb_back_into_the_cap() {
        let rpc = CappedRpc::new(8);
        fetch_adaptive(
            &(rpc.clone() as DynRpc),
            &LogWindow::new(8),
            Address::ZERO,
            0,
            255,
        )
        .await
        .unwrap();
        let calls = rpc.calls.load(Ordering::SeqCst);
        let rejections = rpc.rejections.load(Ordering::SeqCst);
        assert!(
            rejections * 4 < calls,
            "rejections should be a one-off search cost, got {rejections} of {calls}"
        );
    }

    /// A provider that rejects even a single block is not a sizing problem.
    #[tokio::test]
    async fn surfaces_a_cap_it_cannot_satisfy() {
        let rpc = CappedRpc::new(0);
        let err = fetch_adaptive(&(rpc as DynRpc), &LogWindow::new(8), Address::ZERO, 0, 10)
            .await
            .expect_err("cannot shrink below one block");
        assert!(matches!(err, IngesterError::Rpc(RpcError::RangeTooLarge)));
    }

    /// Rate limits belong to the retry layer; handling them here would shrink the
    /// window over a condition unrelated to size.
    #[tokio::test]
    async fn passes_non_range_errors_through() {
        struct Limited;
        #[async_trait]
        impl ChainRpc for Limited {
            async fn tip(&self) -> Result<u64, IngesterError> {
                unimplemented!()
            }
            async fn fetch_logs(
                &self,
                _a: Address,
                _f: u64,
                _t: u64,
            ) -> Result<Vec<Log>, IngesterError> {
                Err(IngesterError::Rpc(RpcError::RateLimited))
            }
            async fn fetch_block_meta(
                &self,
                _b: &[u64],
            ) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
                unimplemented!()
            }
            async fn block_hash_at(&self, _n: u64) -> Result<Option<B256>, IngesterError> {
                unimplemented!()
            }
        }
        let err = fetch_adaptive(
            &(Arc::new(Limited) as DynRpc),
            &LogWindow::new(8),
            Address::ZERO,
            0,
            10,
        )
        .await
        .expect_err("rate limit is not a sizing problem");
        assert!(matches!(err, IngesterError::Rpc(RpcError::RateLimited)));
    }

    /// Without a shared cap every chunk re-asks the provider for the full
    /// span and re-pays the halving search. The cap belongs to the provider, so
    /// the second call must start where the first one left off.
    #[tokio::test]
    async fn the_learned_cap_survives_across_calls() {
        let rpc = CappedRpc::new(8);
        let learned = LogWindow::new(8);
        let dyn_rpc = rpc.clone() as DynRpc;

        fetch_adaptive(&dyn_rpc, &learned, Address::ZERO, 0, 255)
            .await
            .unwrap();
        let first = rpc.rejections.load(Ordering::SeqCst);
        assert!(first > 0, "the first call must pay the search");

        fetch_adaptive(&dyn_rpc, &learned, Address::ZERO, 256, 511)
            .await
            .unwrap();

        assert_eq!(
            rpc.rejections.load(Ordering::SeqCst),
            first,
            "the second call must not re-probe a cap already known"
        );
    }

    /// A fresh window per call is the bug the shared one fixes; pin the contrast
    /// so a refactor that drops the sharing fails here.
    #[tokio::test]
    async fn an_unshared_cap_re_probes_every_call() {
        let rpc = CappedRpc::new(8);
        let dyn_rpc = rpc.clone() as DynRpc;

        fetch_adaptive(&dyn_rpc, &LogWindow::new(8), Address::ZERO, 0, 255)
            .await
            .unwrap();
        let first = rpc.rejections.load(Ordering::SeqCst);

        fetch_adaptive(&dyn_rpc, &LogWindow::new(8), Address::ZERO, 256, 511)
            .await
            .unwrap();

        assert!(
            rpc.rejections.load(Ordering::SeqCst) > first,
            "a fresh window has nothing to remember"
        );
    }

    /// Serves one log per block up to `max_span`, holding each request open long
    /// enough for overlap to be observable under a paused clock.
    struct SlowRpc {
        max_span: u64,
        in_flight: AtomicUsize,
        peak: AtomicUsize,
    }

    impl SlowRpc {
        fn new(max_span: u64) -> Arc<Self> {
            Arc::new(Self {
                max_span,
                in_flight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl ChainRpc for SlowRpc {
        async fn tip(&self) -> Result<u64, IngesterError> {
            unimplemented!("not exercised")
        }
        async fn fetch_logs(
            &self,
            _address: Address,
            from: u64,
            to: u64,
        ) -> Result<Vec<Log>, IngesterError> {
            if to - from + 1 > self.max_span {
                return Err(IngesterError::Rpc(RpcError::RangeTooLarge));
            }
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok((from..=to).map(|_| Log::default()).collect())
        }
        async fn fetch_block_meta(
            &self,
            _blocks: &[u64],
        ) -> Result<HashMap<u64, BlockMeta>, IngesterError> {
            unimplemented!("not exercised")
        }
        async fn block_hash_at(&self, _n: u64) -> Result<Option<B256>, IngesterError> {
            unimplemented!("not exercised")
        }
    }

    /// Once the cap is known the sub-window sizes need no feedback, so walking
    /// them one response at a time is pure latency: a chunk spanning ten caps
    /// took ten round trips to fetch what fits in one wave.
    #[tokio::test(start_paused = true)]
    async fn a_known_cap_is_covered_by_concurrent_windows() {
        let rpc = SlowRpc::new(8);
        let learned = LogWindow::new(4);
        let dyn_rpc = rpc.clone() as DynRpc;

        // First call learns the cap on the serial path.
        fetch_adaptive(&dyn_rpc, &learned, Address::ZERO, 0, 255)
            .await
            .unwrap();
        assert_eq!(
            rpc.peak.load(Ordering::SeqCst),
            1,
            "the search itself must stay serial; each size depends on the last answer"
        );
        rpc.peak.store(0, Ordering::SeqCst);

        let logs = fetch_adaptive(&dyn_rpc, &learned, Address::ZERO, 256, 511)
            .await
            .unwrap();

        assert_eq!(logs.len(), 256, "every block covered exactly once");
        assert_eq!(
            rpc.peak.load(Ordering::SeqCst),
            4,
            "windows must overlap up to the configured concurrency"
        );
    }

    /// The concurrency belongs to the provider, not to one call: a per-call bound
    /// is multiplied by however many backfill chunks are in flight, which is not
    /// the bound the config promises.
    #[tokio::test(start_paused = true)]
    async fn concurrent_calls_share_one_budget() {
        let rpc = SlowRpc::new(8);
        let learned = Arc::new(LogWindow::new(4));
        let dyn_rpc = rpc.clone() as DynRpc;

        fetch_adaptive(&dyn_rpc, &learned, Address::ZERO, 0, 255)
            .await
            .unwrap();
        rpc.peak.store(0, Ordering::SeqCst);

        // Three chunks at once, as the backfill would run them.
        let calls = (0..3u64).map(|i| {
            let (rpc, learned) = (dyn_rpc.clone(), learned.clone());
            async move {
                let from = 1_000 + i * 256;
                fetch_adaptive(&rpc, &learned, Address::ZERO, from, from + 255)
                    .await
                    .unwrap();
            }
        });
        futures::future::join_all(calls).await;

        assert_eq!(
            rpc.peak.load(Ordering::SeqCst),
            4,
            "three concurrent chunks must share the one budget, not get one each"
        );
    }

    /// A cap that can only fall turns any misread error string into a
    /// permanent throttle: the provider's wording is the only evidence there is,
    /// and "rate limit exceeded" used to read as a range cap. Clean windows must
    /// let it climb back.
    #[tokio::test]
    async fn a_cap_recovers_after_enough_clean_windows() {
        let rpc = CappedRpc::new(8);
        let learned = LogWindow::new(8);
        let dyn_rpc = rpc.clone() as DynRpc;

        fetch_adaptive(&dyn_rpc, &learned, Address::ZERO, 0, 255)
            .await
            .unwrap();
        let after_search = learned.cap.limit();
        assert!(after_search < u64::MAX, "the first call must learn a cap");

        // Well past the recovery threshold, all of it served cleanly.
        let span = CONFIRMATIONS_BEFORE_PROBE * after_search;
        fetch_adaptive(&dyn_rpc, &learned, Address::ZERO, 256, 256 + span)
            .await
            .unwrap();

        assert!(
            learned.cap.limit() > after_search,
            "cap stayed pinned at {after_search} after {span} clean blocks"
        );
    }

    /// Only windows at the full cap are evidence about where the cap is. A live
    /// tail scanning a handful of blocks per tick would otherwise confirm its way
    /// to an ever-rising cap without a single query having tested it.
    #[test]
    fn short_windows_are_not_evidence_about_the_cap() {
        let learned = LogWindow::new(1);
        let mut w = Window::new(64, learned.cap.limit());
        w.shrink(&learned.cap);
        let cap = learned.cap.limit();

        for _ in 0..CONFIRMATIONS_BEFORE_PROBE * 4 {
            learned.cap.confirm(cap - 1);
        }

        assert_eq!(
            learned.cap.limit(),
            cap,
            "a window short of the cap says nothing about it"
        );
    }

    /// Probing must be able to lift a cap of one. Halving bottoms out there,
    /// so a growth rule of "add half" would leave the worst case — one block per
    /// request — permanently stuck.
    #[test]
    fn a_probe_lifts_a_cap_of_one() {
        let learned = LogWindow::new(1);
        let mut w = Window::new(2, learned.cap.limit());
        while w.can_shrink() {
            w.shrink(&learned.cap);
        }
        assert_eq!(learned.cap.limit(), 1, "bottomed out");
        for _ in 0..CONFIRMATIONS_BEFORE_PROBE {
            learned.cap.confirm(1);
        }
        assert!(
            learned.cap.limit() > 1,
            "a cap of one must still be able to grow"
        );
    }

    /// A probe that overshoots must fall back to the size already served, not to
    /// half the probe — otherwise every upward probe costs more throughput than
    /// it can win and the cap ratchets down anyway.
    #[test]
    fn an_overshooting_probe_falls_back_to_the_last_good_size() {
        let learned = LogWindow::new(1);
        learned.cap.confirm(8);
        let mut w = Window { size: 12 };
        w.shrink(&learned.cap);
        assert_eq!(w.size, 8, "must not undershoot a size the provider served");
        assert_eq!(learned.cap.limit(), 8);
    }

    /// The floor is evidence, not a ratchet: a provider that genuinely tightens
    /// its plan rejects a size it used to serve, and the search has to be free to
    /// keep going below it.
    #[test]
    fn a_provider_that_tightens_can_still_be_narrowed_past_the_floor() {
        let learned = LogWindow::new(1);
        learned.cap.confirm(8);
        let mut w = Window { size: 8 };
        w.shrink(&learned.cap);
        assert_eq!(
            w.size, 4,
            "a rejection at the known-good size is not an overshoot"
        );
        w.shrink(&learned.cap);
        assert_eq!(w.size, 2);
    }

    #[test]
    fn a_known_cap_is_not_exceeded_by_the_first_request() {
        let w = Window::new(50_000, 2_000);
        assert_eq!(w.size, 2_000, "start at the cap, not at the span");
    }

    #[test]
    fn growth_never_exceeds_a_learned_cap() {
        let learned = LogWindow::new(8);
        let mut w = Window::new(1000, learned.cap.limit());
        w.shrink(&learned.cap);
        for _ in 0..10 {
            w.grow(u64::MAX, learned.cap.limit());
            assert!(w.size <= learned.cap.limit(), "grew past the learned cap");
        }
    }

    /// Shrinking must publish the cap, not just narrow this window — otherwise
    /// the next call re-runs the whole search.
    #[test]
    fn shrinking_publishes_the_cap() {
        let learned = LogWindow::new(8);
        let mut w = Window::new(1000, learned.cap.limit());
        w.shrink(&learned.cap);
        assert_eq!(learned.cap.limit(), w.size);
    }

    #[test]
    fn shrinking_bottoms_out_at_one_block() {
        let learned = LogWindow::new(8);
        let mut w = Window::new(4, learned.cap.limit());
        while w.can_shrink() {
            w.shrink(&learned.cap);
        }
        assert_eq!(w.size, 1);
    }
}
