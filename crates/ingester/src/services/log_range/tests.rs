//! The adaptive window against scripted providers.

use super::window::{CONFIRMATIONS_BEFORE_PROBE, Window};
use super::*;
use crate::adapters::rpc::ChainRpc;
use crate::domain::models::BlockMeta;
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
