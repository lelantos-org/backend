//! An estimated annual rate for each yield-bearing asset, for `/v1/assets`.
//!
//! The pool publishes no rate. `yieldState` returns an index and no timestamp,
//! and `asset_yield` is overwritten on every indexer pass, so there is no history
//! anywhere to difference. A rate has to be measured, and the only question is
//! what to measure it against.
//!
//! There are two ways to get one, and this module uses both, in this order:
//!
//!   1. **The recorded index.** Every pass copies the current index into
//!      `asset_yield_sample`, so once the history reaches back a window the rate
//!      is a subtraction against a row this deployment wrote down. It needs no
//!      archive state and no RPC, and the index is the figure a note is actually
//!      worth — already net of the performance fee and the idle buffer — so
//!      differencing two of them is exact rather than corrected.
//!
//!   2. **The venue's vault.** Until that history exists, the vault is the older
//!      object: an ERC-4626 vault the pool was pointed at, live long before the
//!      pool was deployed, whose share price carries exactly the history the pool
//!      lacks. Two readings of `convertToAssets` a window apart answer on the day
//!      the pool ships — at the cost of an archive node, and of correcting for
//!      what the pool keeps rather than measuring it.
//!
//! The vault path is a bootstrap. It covers the days before the history fills,
//! and stops being consulted the moment it has. On a node without archive state
//! it never works at all, which is exactly why path 1 exists.
//!
//! Two corrections stand between the *vault's* growth and what a note holder
//! gets, and the vault path applies both. Path 1 needs neither:
//!
//!   - **The performance fee.** The pool skims `perf_bps` of the yield.
//!   - **The buffer.** `buffer_bps` of custody is deliberately left idle for
//!     withdrawals, and idle assets earn nothing.
//!
//! What comes out either way is an estimate and is published as one. It is what
//! happened over the window, not what will happen next, and the window travels
//! with it so a client can say so.
//!
//! This is the service half: it reads, decides and stores. The arithmetic is
//! [`crate::domain::apy`], the chain reads are [`crate::adapters::venue`], and
//! the tick loop that elects one replica per chain to run any of it is
//! [`crate::handlers::worker::venue_apy`].

mod recorded;
mod vault;

use crate::adapters::rpc::RpcEndpoint;
use crate::adapters::venue::VenueReader;
use crate::domain::apy::{MAX_WINDOW_SECONDS, MIN_WINDOW_SECONDS};
use crate::repositories::{asset_yield, yield_samples};
use asset_registry::{ApyEstimate, AssetRow};
use database::DbPool;
use std::collections::HashMap;
use tracing::{debug, warn};

/// One chain's readings, refreshed on an interval.
pub struct VenueApyWorker {
    chain_id: i64,
    pool: DbPool,
    venue: VenueReader,
}

impl VenueApyWorker {
    pub fn new(chain_id: i64, pool: DbPool, rpc: &RpcEndpoint) -> Self {
        Self {
            chain_id,
            pool,
            venue: VenueReader::new(chain_id, rpc),
        }
    }

    /// Re-measure every yield asset in `assets`.
    ///
    /// One pass reads three block headers and two vault calls per asset. Failures
    /// are per asset and never fatal: this feeds a badge, and every caller
    /// downstream has something to render without it.
    pub async fn refresh(&mut self, assets: &[AssetRow]) {
        // A chain with no venue has nothing to record and nothing to measure;
        // without this it would still spend two statements every pass, forever.
        if assets.iter().all(|a| a.venue.is_none()) {
            return;
        }

        self.record_history().await;

        // Two passes rather than one with a lazily resolved window inside it. The
        // recorded path answers from a map already in hand, so the assets it
        // cannot serve are known before the vault path is consulted at all — and
        // on a deployment whose history has filled that list is empty and the
        // vault's three header reads are never spent.
        let samples = self.recorded_windows().await;

        let mut needs_vault = Vec::new();
        for a in assets.iter().filter(|a| a.venue.is_some()) {
            match recorded::rate(a, samples.get(&a.asset_id_u64)) {
                Some(est) => self.store(a, est).await,
                None => needs_vault.push(a),
            }
        }
        if needs_vault.is_empty() {
            return;
        }

        let Some((head, target)) = self.window().await else {
            debug!(
                chain_id = self.chain_id,
                "venue apy: no vault window available"
            );
            return;
        };
        for a in needs_vault {
            match self
                .vault_rate(head.0, target.0, head.1 - target.1, a)
                .await
            {
                Some(est) => self.store(a, est).await,
                None => debug!(
                    chain_id = self.chain_id,
                    asset_id = a.asset_id(),
                    "venue apy: not measurable"
                ),
            }
        }
    }

    /// Copy the current readings into the history, then thin what is there.
    ///
    /// Written down before anything is read back, so the very first pass on a
    /// new deployment lays the anchor the later ones measure against.
    async fn record_history(&self) {
        if let Err(e) = yield_samples::record(&self.pool, self.chain_id).await {
            warn!(chain_id = self.chain_id, error = %e, "venue apy: sample not recorded");
        }
        // Thinned rather than cut off at `MAX_WINDOW_SECONDS`: the history is
        // also read for note cost basis, whose lookback is the age of the oldest
        // unspent note and so unbounded. The tiers keep this window at hourly
        // resolution, which is finer than the two-point measurement below needs.
        if let Err(e) = yield_samples::thin(&self.pool, self.chain_id).await {
            warn!(chain_id = self.chain_id, error = %e, "venue apy: thin failed");
        }
    }

    /// The oldest in-window sample of each asset on this chain.
    ///
    /// An unreadable history is an empty map rather than a failure: the vault
    /// path is still there to answer, which is the whole reason it is kept.
    async fn recorded_windows(&self) -> HashMap<i64, yield_samples::Sample> {
        match yield_samples::windows(
            &self.pool,
            self.chain_id,
            MIN_WINDOW_SECONDS,
            MAX_WINDOW_SECONDS,
        )
        .await
        {
            Ok(samples) => samples,
            Err(e) => {
                warn!(chain_id = self.chain_id, error = %e, "venue apy: history unreadable");
                HashMap::new()
            }
        }
    }

    /// Persist one asset's estimate.
    ///
    /// Stored rather than cached in this process: the service that measures is
    /// not necessarily the one that answers a request for it. A failed write
    /// leaves the previous estimate standing until it ages out, which is the
    /// same thing a failed measurement does.
    async fn store(&self, a: &AssetRow, est: ApyEstimate) {
        match asset_yield::store_estimate(&self.pool, self.chain_id, a.asset_id_u64, est).await {
            Ok(true) => {}
            Ok(false) => warn!(
                chain_id = self.chain_id,
                asset_id = a.asset_id(),
                "venue apy: no asset_yield row to store the estimate on"
            ),
            Err(e) => warn!(
                chain_id = self.chain_id,
                asset_id = a.asset_id(),
                error = %e,
                "venue apy: estimate not stored"
            ),
        }
    }
}
