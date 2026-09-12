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

use crate::adapters::rpc::RpcEndpoint;
use crate::adapters::venue::{BlockRef, ShareReadError, VenueReader};
use crate::domain::apy::{
    MAX_WINDOW_SECONDS, MIN_WINDOW_SECONDS, annualize_bps, net_of_pool, window_start_block,
};
use crate::repositories::{asset_yield, yield_samples};
use alloy::rpc::types::BlockNumberOrTag;
use asset_registry::{AssetRow, bigdecimal_to_u256};
use database::DbPool;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Blocks back for the block-time probe. Only sizes the estimate of where the
/// window starts, so it does not have to be exact — see
/// [`crate::domain::apy::window_start_block`].
const PROBE_BLOCKS: u64 = 5_000;

/// One asset's estimated rate, and the window it was measured over.
///
/// Defined next to the row it is stored on, so the writer here and every reader
/// of the catalog agree on the shape without this module being in the way.
pub use asset_registry::ApyEstimate;

/// The rate from this deployment's own record of the index.
///
/// The preferred path, and the only one that keeps working on a node without
/// archive state. Nothing is corrected for the pool's cut here: the index is
/// already what a note is worth, so two of them difference to what a holder
/// earned rather than to what the venue paid.
///
/// Free-standing and given its sample rather than fetching one: the whole chain's
/// history arrives in a single query, so this is arithmetic with no I/O in it.
fn recorded_rate(a: &AssetRow, sample: Option<&yield_samples::Sample>) -> Option<ApyEstimate> {
    let sample = sample?;
    let now = bigdecimal_to_u256(a.index_ray.as_ref()?).ok()?;
    let then = bigdecimal_to_u256(&sample.index_ray).ok()?;
    Some(ApyEstimate {
        bps: annualize_bps(now, then, sample.elapsed_s)?,
        window_s: sample.elapsed_s,
    })
}

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
            match recorded_rate(a, samples.get(&a.asset_id_u64)) {
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

    /// `(head, window start)` as `(block number, unix seconds)`.
    async fn window(&self) -> Option<(BlockRef, BlockRef)> {
        let head = self.venue.block(BlockNumberOrTag::Latest).await?;
        let probe_at = head.0.saturating_sub(PROBE_BLOCKS);
        let probe = self.venue.block(BlockNumberOrTag::Number(probe_at)).await?;
        let start = window_start_block(head.0, head.1, probe.0, probe.1)?;
        let target = self.venue.block(BlockNumberOrTag::Number(start)).await?;
        Some((head, target))
    }

    /// One asset's estimate from the venue's vault, or `None` if either reading
    /// failed.
    ///
    /// The bootstrap path: it answers before this deployment has a history of its
    /// own, and needs an RPC serving state a window back. See the module header
    /// for why it is second choice once [`recorded_rate`] can answer.
    async fn vault_rate(
        &mut self,
        head: u64,
        target: u64,
        elapsed: i64,
        asset: &AssetRow,
    ) -> Option<ApyEstimate> {
        let (now, then) = match self
            .venue
            .share_prices(asset.venue_address()?, head, target)
            .await
        {
            Ok(pair) => pair,
            // Overwhelmingly an RPC without state that far back, which is a
            // property of the endpoint rather than of the asset. Logged once per
            // asset per pass, at debug: on a pruned node this is every asset,
            // every pass, forever.
            Err(ShareReadError::Readings { head_ok, window_ok }) => {
                debug!(
                    chain_id = self.chain_id,
                    asset_id = asset.asset_id(),
                    head_ok,
                    window_ok,
                    "venue apy: vault read failed"
                );
                return None;
            }
            Err(ShareReadError::Vault) => return None,
        };

        let gross = annualize_bps(now, then, elapsed)?;
        Some(ApyEstimate {
            bps: net_of_pool(
                gross,
                asset.perf_bps.unwrap_or(0),
                asset.buffer_bps.unwrap_or(0),
            ),
            window_s: elapsed,
        })
    }
}
