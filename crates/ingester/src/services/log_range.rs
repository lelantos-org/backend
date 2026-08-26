//! Adaptive `eth_getLogs` windowing.
//!
//! Providers cap how much one query may return, by block span, by response size
//! or both, and disagree on the limit and on how they report reaching it. Rather
//! than a per-provider constant, the window is probed: shrink on rejection, grow
//! on success.

use crate::adapters::DynRpc;
use crate::domain::error::{IngesterError, RpcError};
use alloy::primitives::Address;
use alloy::rpc::types::eth::Log;

/// The window size search.
///
/// Split out from the fetch loop so the sizing rule is testable without a
/// provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    size: u64,
    /// Largest size known to be rejected, minus one. Once the provider has
    /// refused a size, the window never climbs back to it.
    ceiling: u64,
}

impl Window {
    fn new(span: u64) -> Self {
        Self {
            size: span.max(1),
            ceiling: u64::MAX,
        }
    }

    /// Whether the window can shrink further, or a single block is already too
    /// much.
    fn can_shrink(&self) -> bool {
        self.size > 1
    }

    fn shrink(&mut self) {
        self.size = (self.size / 2).max(1);
        // Remember the cap: doubling straight back into it after every shrink
        // would make half of all requests fail.
        self.ceiling = self.size;
    }

    fn grow(&mut self, remaining: u64) {
        self.size = self
            .size
            .saturating_mul(2)
            .min(remaining)
            .min(self.ceiling)
            .max(1);
    }
}

/// Fetch every matching log in `[from, to]`, narrowing the query window to
/// whatever the provider will actually serve.
///
/// Anything that is not a range cap propagates: rate limits and transport errors
/// belong to the retry layer.
pub async fn fetch_adaptive(
    rpc: &DynRpc,
    address: Address,
    from: u64,
    to: u64,
) -> Result<Vec<Log>, IngesterError> {
    let mut window = Window::new(to.saturating_sub(from).saturating_add(1));
    let mut cursor = from;
    let mut acc = Vec::new();

    while cursor <= to {
        let end = cursor.saturating_add(window.size - 1).min(to);
        match rpc.fetch_logs(address, cursor, end).await {
            Ok(mut logs) => {
                acc.append(&mut logs);
                cursor = end + 1;
                window.grow(to.saturating_sub(cursor).saturating_add(1));
            }
            Err(IngesterError::Rpc(RpcError::RangeTooLarge)) if window.can_shrink() => {
                window.shrink();
            }
            Err(e) => return Err(e),
        }
    }
    Ok(acc)
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
        let logs = fetch_adaptive(&(rpc.clone() as DynRpc), Address::ZERO, 0, 99)
            .await
            .expect("range cap is recoverable");
        assert_eq!(logs.len(), 100, "every block covered exactly once");
    }

    /// Without a learned ceiling the window doubles straight back into the cap
    /// after every success, so roughly half of all requests fail.
    #[tokio::test]
    async fn does_not_climb_back_into_the_cap() {
        let rpc = CappedRpc::new(8);
        fetch_adaptive(&(rpc.clone() as DynRpc), Address::ZERO, 0, 255)
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
        let err = fetch_adaptive(&(rpc as DynRpc), Address::ZERO, 0, 10)
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
        let err = fetch_adaptive(&(Arc::new(Limited) as DynRpc), Address::ZERO, 0, 10)
            .await
            .expect_err("rate limit is not a sizing problem");
        assert!(matches!(err, IngesterError::Rpc(RpcError::RateLimited)));
    }

    #[test]
    fn growth_never_exceeds_a_learned_ceiling() {
        let mut w = Window::new(1000);
        w.shrink();
        let ceiling = w.ceiling;
        for _ in 0..10 {
            w.grow(u64::MAX);
            assert!(w.size <= ceiling, "grew past the learned ceiling");
        }
    }

    #[test]
    fn shrinking_bottoms_out_at_one_block() {
        let mut w = Window::new(4);
        while w.can_shrink() {
            w.shrink();
        }
        assert_eq!(w.size, 1);
    }
}
