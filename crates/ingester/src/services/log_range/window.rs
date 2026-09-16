//! What this chain's provider is believed to serve, and the per-call search
//! that narrows toward it.

use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Semaphore, SemaphorePermit};

/// Full-size windows the provider must serve before the learned cap is probed
/// upward.
///
/// The cap is inferred from provider error strings, so a string that is misread
/// pins every later query below what the provider actually serves. A cap that
/// can only fall makes that permanent for the life of the process; probing makes
/// it self-healing. At this threshold the probe costs under one percent of
/// requests, and a wrongly-lowered cap climbs back out within a few hundred
/// windows.
pub(super) const CONFIRMATIONS_BEFORE_PROBE: u64 = 128;

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
pub(super) struct LearnedCap {
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

    pub(super) fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Record that `rejected` was refused, and answer with the span to retry at.
    ///
    /// Never raises `limit`: a size that once failed must not be retried because
    /// another chunk happened to succeed at it.
    pub(super) fn refuse(&self, rejected: u64) -> u64 {
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
    pub(super) fn confirm(&self, span: u64) {
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
    pub(super) cap: LearnedCap,
    permits: Semaphore,
    pub(super) concurrency: usize,
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
    pub(super) async fn permit(&self) -> SemaphorePermit<'_> {
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
pub(super) struct Window {
    pub(super) size: u64,
}

impl Window {
    /// Start at whatever the span asks for, capped by what the provider is
    /// already known to refuse.
    pub(super) fn new(span: u64, limit: u64) -> Self {
        Self {
            size: span.min(limit).max(1),
        }
    }

    /// Whether the window can shrink further, or a single block is already too
    /// much.
    pub(super) fn can_shrink(&self) -> bool {
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
    pub(super) fn shrink(&mut self, cap: &LearnedCap) {
        self.size = cap.refuse(self.size);
    }

    pub(super) fn grow(&mut self, remaining: u64, limit: u64) {
        self.size = self.size.saturating_mul(2).min(remaining).min(limit).max(1);
    }
}
