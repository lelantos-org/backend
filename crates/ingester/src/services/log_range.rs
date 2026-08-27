//! Adaptive `eth_getLogs` windowing.
//!
//! Providers cap how much one query may return, by block span, by response size
//! or both, and disagree on the limit and on how they report reaching it. Rather
//! than a per-provider constant, the window is probed: shrink on rejection, grow
//! on success.

use crate::adapters::DynRpc;
use crate::domain::error::{IngesterError, RpcError};
use crate::domain::models::RawEvent;
use crate::services::decode::{distinct_blocks, logs_to_rows};
use alloy::primitives::Address;
use alloy::rpc::types::eth::Log;
use shared::metrics::{ingest_stage, timed_ingest_stage};
use std::sync::atomic::{AtomicU64, Ordering};

/// The `eth_getLogs` range cap this chain's provider actually enforces, learned
/// once and shared by every fetch against it.
///
/// The cap is a property of the provider and its plan, not of one query, so
/// re-probing it per call means every backfill chunk repeats the same halving
/// search — and the usual rejection is a timeout, where the provider burns
/// `rpc_timeout_ms` before refusing. Held here so the search is paid once per
/// process.
///
/// Monotonically decreasing, so concurrent backfill chunks converge on the cap
/// without coordinating.
#[derive(Debug)]
pub struct LogWindow {
    /// Largest size known to be rejected, minus one. `u64::MAX` until the
    /// provider first refuses anything.
    ceiling: AtomicU64,
}

impl Default for LogWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl LogWindow {
    pub fn new() -> Self {
        Self {
            ceiling: AtomicU64::new(u64::MAX),
        }
    }

    fn ceiling(&self) -> u64 {
        self.ceiling.load(Ordering::Relaxed)
    }

    /// Record that `size` was refused. Never raises the ceiling: a size that
    /// once failed must not be retried because another chunk happened to
    /// succeed at it.
    fn lower_to(&self, size: u64) {
        self.ceiling.fetch_min(size, Ordering::Relaxed);
    }
}

/// The window size search.
///
/// Split out from the fetch loop so the sizing rule is testable without a
/// provider. Holds only the current size: the ceiling lives in [`LogWindow`], so
/// there is one authoritative copy and a shrink cannot learn something the next
/// call forgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    size: u64,
}

impl Window {
    /// Start at whatever the span asks for, capped by what the provider is
    /// already known to refuse.
    fn new(span: u64, ceiling: u64) -> Self {
        Self {
            size: span.min(ceiling).max(1),
        }
    }

    /// Whether the window can shrink further, or a single block is already too
    /// much.
    fn can_shrink(&self) -> bool {
        self.size > 1
    }

    /// Halve, and publish the new size as a cap the provider has refused above.
    ///
    /// Publishing is what stops the window doubling straight back into the cap
    /// after every shrink, which would make half of all requests fail — and what
    /// lets a sibling chunk skip the search entirely.
    fn shrink(&mut self, learned: &LogWindow) {
        self.size = (self.size / 2).max(1);
        learned.lower_to(self.size);
    }

    fn grow(&mut self, remaining: u64, ceiling: u64) {
        self.size = self
            .size
            .saturating_mul(2)
            .min(remaining)
            .min(ceiling)
            .max(1);
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
    let mut window = Window::new(span, learned.ceiling());
    let mut cursor = from;
    let mut acc = Vec::new();

    while cursor <= to {
        let end = cursor.saturating_add(window.size - 1).min(to);
        match rpc.fetch_logs(address, cursor, end).await {
            Ok(mut logs) => {
                // Taking the first response whole keeps the common case — one
                // window covering the whole range — free of a copy.
                if acc.is_empty() {
                    acc = logs;
                } else {
                    acc.append(&mut logs);
                }
                cursor = end + 1;
                // Re-read the ceiling rather than reusing the one sampled at
                // entry: a sibling chunk may have found the cap while this one
                // was in flight, and the first backfill wave would otherwise pay
                // the search once per concurrent chunk.
                window.grow(
                    to.saturating_sub(cursor).saturating_add(1),
                    learned.ceiling(),
                );
            }
            Err(IngesterError::Rpc(RpcError::RangeTooLarge)) if window.can_shrink() => {
                window.shrink(learned);
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
            &LogWindow::new(),
            Address::ZERO,
            0,
            99,
        )
        .await
        .expect("range cap is recoverable");
        assert_eq!(logs.len(), 100, "every block covered exactly once");
    }

    /// Without a learned ceiling the window doubles straight back into the cap
    /// after every success, so roughly half of all requests fail.
    #[tokio::test]
    async fn does_not_climb_back_into_the_cap() {
        let rpc = CappedRpc::new(8);
        fetch_adaptive(
            &(rpc.clone() as DynRpc),
            &LogWindow::new(),
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
        let err = fetch_adaptive(&(rpc as DynRpc), &LogWindow::new(), Address::ZERO, 0, 10)
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
            &LogWindow::new(),
            Address::ZERO,
            0,
            10,
        )
        .await
        .expect_err("rate limit is not a sizing problem");
        assert!(matches!(err, IngesterError::Rpc(RpcError::RateLimited)));
    }

    /// Without a shared ceiling every chunk re-asks the provider for the full
    /// span and re-pays the halving search. The cap belongs to the provider, so
    /// the second call must start where the first one left off.
    #[tokio::test]
    async fn the_learned_ceiling_survives_across_calls() {
        let rpc = CappedRpc::new(8);
        let learned = LogWindow::new();
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
    async fn an_unshared_ceiling_re_probes_every_call() {
        let rpc = CappedRpc::new(8);
        let dyn_rpc = rpc.clone() as DynRpc;

        fetch_adaptive(&dyn_rpc, &LogWindow::new(), Address::ZERO, 0, 255)
            .await
            .unwrap();
        let first = rpc.rejections.load(Ordering::SeqCst);

        fetch_adaptive(&dyn_rpc, &LogWindow::new(), Address::ZERO, 256, 511)
            .await
            .unwrap();

        assert!(
            rpc.rejections.load(Ordering::SeqCst) > first,
            "a fresh window has nothing to remember"
        );
    }

    #[test]
    fn a_known_cap_is_not_exceeded_by_the_first_request() {
        let w = Window::new(50_000, 2_000);
        assert_eq!(w.size, 2_000, "start at the cap, not at the span");
    }

    #[test]
    fn growth_never_exceeds_a_learned_ceiling() {
        let learned = LogWindow::new();
        let mut w = Window::new(1000, learned.ceiling());
        w.shrink(&learned);
        for _ in 0..10 {
            w.grow(u64::MAX, learned.ceiling());
            assert!(w.size <= learned.ceiling(), "grew past the learned ceiling");
        }
    }

    /// Shrinking must publish the cap, not just narrow this window — otherwise
    /// the next call re-runs the whole search.
    #[test]
    fn shrinking_publishes_the_cap() {
        let learned = LogWindow::new();
        let mut w = Window::new(1000, learned.ceiling());
        w.shrink(&learned);
        assert_eq!(learned.ceiling(), w.size);
    }

    #[test]
    fn shrinking_bottoms_out_at_one_block() {
        let learned = LogWindow::new();
        let mut w = Window::new(4, learned.ceiling());
        while w.can_shrink() {
            w.shrink(&learned);
        }
        assert_eq!(w.size, 1);
    }
}
