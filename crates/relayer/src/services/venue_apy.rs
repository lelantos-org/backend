//! An estimated annual rate for each yield-bearing asset, for `/chains`.
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

use crate::adapters::abi::{IERC4626, IYieldVenue};
use crate::adapters::numeric::bigdecimal_to_u256;
use crate::adapters::rpc::{HttpTransport, RpcEndpoint};
use crate::app::config::BPS_DENOMINATOR;
use crate::repositories::assets::AssetRow;
use crate::repositories::yield_samples;
use crate::services::asset_registry::AssetRegistry;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::types::BlockNumberOrTag;
use database::DbPool;
use moka::future::Cache;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Seconds in a year, for the exponent. 365 days: a venue has no calendar, and a
/// fixed year keeps the figure reproducible from the two samples alone.
const YEAR_SECONDS: f64 = 365.0 * 24.0 * 60.0 * 60.0;

/// How far back the older sample is taken.
///
/// A week: long enough that one lumpy accrual does not become the rate, short
/// enough to describe the venue as it is now. Published alongside the figure —
/// a rate without its window is not a claim anyone can check.
pub const WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;

/// The shortest measured window worth annualizing.
///
/// The exponent is `year / window`, so a short window multiplies whatever it
/// caught — a single accrual, a rounding — by a large number. Below this the
/// samples are dropped rather than dressed up as a rate.
const MIN_WINDOW_SECONDS: i64 = 2 * 24 * 60 * 60;

/// Above this the readings are treated as garbage rather than as a rate: a
/// reindexed vault, a migration, or a window that straddled one. 10,000%.
const MAX_APY_BPS: i64 = 1_000_000;

/// The widest span the recorded history is allowed to answer over, and how long
/// a sample is kept.
///
/// Twice the window, so a gap in the record — a stopped relayer, a stalled
/// indexer — is survivable rather than blanking the figure, while a rate is
/// never measured over a quarter and called current.
const MAX_WINDOW_SECONDS: i64 = 2 * WINDOW_SECONDS;

/// Blocks back for the block-time probe. Only sizes the estimate of where the
/// window starts, so it does not have to be exact — see [`window_start_block`].
const PROBE_BLOCKS: u64 = 5_000;

/// How often the worker re-measures. A week-long window does not move in an
/// hour, and each pass costs archive reads.
const REFRESH: Duration = Duration::from_secs(30 * 60);

/// One asset's estimated rate, and the window it was measured over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApyEstimate {
    /// Annualized, in basis points, net of the pool's performance fee and
    /// buffer. Negative after a venue loss, which is a real outcome.
    pub bps: i32,
    /// Seconds actually spanned by the two samples — not [`WINDOW_SECONDS`],
    /// which is only what was aimed for.
    pub window_s: i64,
}

/// Estimates by `(chain_id, asset_id)`, refreshed by the worker and read by
/// `/chains`.
///
/// A cache rather than a store: an entry that stops being refreshed — an RPC
/// that lost its archive state, a venue that went away — expires rather than
/// being served indefinitely as though it were still measured.
pub type VenueApyCache = Cache<(i64, u64), ApyEstimate>;

/// Entries live somewhat longer than the refresh interval, so a single failed
/// pass does not blank every badge in every wallet.
pub fn new_cache() -> VenueApyCache {
    shared::cache::build(256, REFRESH * 3)
}

/// The ratio of two readings, as a float.
///
/// Divided as integers first. Both readings are token amounts that can exceed
/// `u128`, and converting each to `f64` before dividing would round both
/// operands — the one rounding that would survive into the answer.
fn ratio(now: U256, then: U256) -> Option<f64> {
    const SCALE: u64 = 1_000_000_000_000;
    if then.is_zero() || now.is_zero() {
        return None;
    }
    let scaled = now.checked_mul(U256::from(SCALE))?.checked_div(then)?;
    let scaled = u128::try_from(scaled).ok()?;
    Some(scaled as f64 / SCALE as f64)
}

/// The vault's own annualized growth between two share-price readings, in bps.
///
/// `None` whenever the pair cannot support a rate: too short a window, a zero
/// reading, or a result too large to be one. Gross of the pool's cut; see
/// [`net_of_pool`].
pub fn annualize_bps(now: U256, then: U256, elapsed_s: i64) -> Option<i32> {
    if elapsed_s < MIN_WINDOW_SECONDS {
        return None;
    }
    let r = ratio(now, then)?;
    let apy = r.powf(YEAR_SECONDS / elapsed_s as f64) - 1.0;
    if !apy.is_finite() {
        return None;
    }
    let bps = (apy * f64::from(BPS_DENOMINATOR)).round() as i64;
    if bps > MAX_APY_BPS {
        return None;
    }
    // The floor is the only rate a total loss can produce, and it is a real one.
    Some(bps.max(-i64::from(BPS_DENOMINATOR)) as i32)
}

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

/// The vault's growth, less what the pool keeps of it.
///
/// `perf_bps` is skimmed off the yield; `buffer_bps` of custody is held idle for
/// withdrawals and earns nothing. Both shrink what reaches a note holder, and
/// both are bounded on chain, so neither can invert the sign here.
///
/// An approximation, deliberately: the buffer is a target the pool drifts around
/// rather than a constant, and a loss is not rebated by the performance fee. It
/// is applied to a loss all the same — reporting a loss as smaller than the
/// vault's would be the flattering direction, and the buffer genuinely damps
/// both.
pub fn net_of_pool(gross_bps: i32, perf_bps: i16, buffer_bps: i16) -> i32 {
    let whole = BPS_DENOMINATOR as i16;
    let keep = |bps: i16| 1.0 - (f64::from(bps.clamp(0, whole)) / f64::from(BPS_DENOMINATOR));
    (gross_bps as f64 * keep(perf_bps) * keep(buffer_bps)).round() as i32
}

/// The block the window starts at, estimated from a probe's block time.
///
/// An estimate only: the block it names then has its own timestamp read, and
/// that is what the rate is computed against. `None` when the probe says nothing
/// usable, or when the chain is not yet a window old.
pub fn window_start_block(
    head_number: u64,
    head_seconds: i64,
    probe_number: u64,
    probe_seconds: i64,
) -> Option<u64> {
    let blocks = head_number.checked_sub(probe_number)?;
    let seconds = head_seconds - probe_seconds;
    if blocks == 0 || seconds <= 0 {
        return None;
    }
    let per_block = seconds as f64 / blocks as f64;
    let back = (WINDOW_SECONDS as f64 / per_block).round();
    if !back.is_finite() || back <= 0.0 {
        return None;
    }
    let back = back as u64;
    if head_number <= back {
        return None;
    }
    Some(head_number - back)
}

/// One chain's readings, refreshed on an interval.
pub struct VenueApyWorker {
    chain_id: i64,
    pool: DbPool,
    provider: RootProvider<HttpTransport>,
    cache: VenueApyCache,
    /// `venue -> (vault, share decimals)`, resolved once. A venue is pinned to
    /// its vault at construction and cannot be re-pointed, and a vault's decimals
    /// are immutable, so neither ever needs invalidating.
    vaults: HashMap<Address, (Address, u8)>,
}

impl VenueApyWorker {
    pub fn new(chain_id: i64, pool: DbPool, rpc: &RpcEndpoint, cache: VenueApyCache) -> Self {
        Self {
            chain_id,
            pool,
            provider: ProviderBuilder::new().on_client(rpc.client()),
            cache,
            vaults: HashMap::new(),
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

        // Written down before anything is read back, so the very first pass on a
        // new deployment lays the anchor the later ones measure against.
        if let Err(e) = yield_samples::record(&self.pool, self.chain_id).await {
            warn!(chain_id = self.chain_id, error = %e, "venue apy: sample not recorded");
        }
        if let Err(e) = yield_samples::prune(&self.pool, self.chain_id, MAX_WINDOW_SECONDS).await {
            warn!(chain_id = self.chain_id, error = %e, "venue apy: prune failed");
        }

        // Two passes rather than one with a lazily resolved window inside it. The
        // recorded path answers from a map already in hand, so the assets it
        // cannot serve are known before the vault path is consulted at all — and
        // on a deployment whose history has filled that list is empty and the
        // vault's three header reads are never spent.
        let samples = match yield_samples::windows(
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
        };

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

    async fn store(&self, a: &AssetRow, est: ApyEstimate) {
        self.cache.insert((self.chain_id, a.asset_id()), est).await;
    }

    /// `(head, window start)` as `(block number, unix seconds)`.
    async fn window(&self) -> Option<((u64, i64), (u64, i64))> {
        let head = self.block(BlockNumberOrTag::Latest).await?;
        let probe_at = head.0.saturating_sub(PROBE_BLOCKS);
        let probe = self.block(BlockNumberOrTag::Number(probe_at)).await?;
        let start = window_start_block(head.0, head.1, probe.0, probe.1)?;
        let target = self.block(BlockNumberOrTag::Number(start)).await?;
        Some((head, target))
    }

    async fn block(&self, at: BlockNumberOrTag) -> Option<(u64, i64)> {
        /// Headers only. The worker reads a number and a timestamp; pulling
        /// every transaction body of a full block to get them would be the
        /// bulk of the response.
        const HYDRATE_TXS: bool = false;

        match self.provider.get_block_by_number(at, HYDRATE_TXS).await {
            Ok(Some(b)) => Some((b.header.number, b.header.timestamp as i64)),
            Ok(None) => None,
            Err(e) => {
                warn!(chain_id = self.chain_id, error = %e, "venue apy: block read failed");
                None
            }
        }
    }

    /// One asset's estimate from the venue's vault, or `None` if either reading
    /// failed.
    ///
    /// The bootstrap path: it answers before this deployment has a history of its
    /// own, and needs an RPC serving state a window back. See the module header
    /// for why it is second choice once [`Self::recorded_rate`] can answer.
    async fn vault_rate(
        &mut self,
        head: u64,
        target: u64,
        elapsed: i64,
        asset: &AssetRow,
    ) -> Option<ApyEstimate> {
        let (vault, decimals) = self.vault_of(asset.venue_address()?).await?;
        let vault = IERC4626::new(vault, &self.provider);

        // One whole share, times a million. `convertToAssets` answers in the
        // asset's own decimals, which for a 6-decimal token leaves a week of
        // growth in the last three digits; the multiplier buys six more, and a
        // ratio of two readings of the same probe is unaffected by its size.
        let probe = U256::from(10u64).checked_pow(U256::from(u32::from(decimals) + 6))?;

        // Joined: the two readings are independent once the probe exists, and
        // each is an archive call whose latency is the bulk of a pass. Bound
        // first — the builders are temporaries the futures borrow from.
        let at_head = vault.convertToAssets(probe).block(head.into());
        let at_target = vault.convertToAssets(probe).block(target.into());
        let (now, then) = tokio::join!(at_head.call(), at_target.call());
        let (now, then) = match (now, then) {
            (Ok(n), Ok(t)) => (n._0, t._0),
            // Overwhelmingly an RPC without state that far back, which is a
            // property of the endpoint rather than of the asset. Logged once per
            // asset per pass, at debug: on a pruned node this is every asset,
            // every pass, forever.
            (n, t) => {
                debug!(
                    chain_id = self.chain_id,
                    asset_id = asset.asset_id(),
                    head_ok = n.is_ok(),
                    window_ok = t.is_ok(),
                    "venue apy: vault read failed"
                );
                return None;
            }
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

    /// The vault behind a venue and its share decimals, read once and
    /// remembered. Both immutable on chain, so re-reading them every pass would
    /// spend a round trip to be told the same thing.
    async fn vault_of(&mut self, venue: Address) -> Option<(Address, u8)> {
        if let Some(v) = self.vaults.get(&venue) {
            return Some(*v);
        }
        let vault = IYieldVenue::new(venue, &self.provider)
            .VAULT()
            .call()
            .await
            .ok()?
            ._0;
        let decimals = IERC4626::new(vault, &self.provider)
            .decimals()
            .call()
            .await
            .ok()?
            ._0;
        self.vaults.insert(venue, (vault, decimals));
        Some((vault, decimals))
    }
}

/// Drive one chain's estimates on a fixed interval.
///
/// Ticks are skipped rather than queued when one runs long. Unlike the flush
/// worker there is no fatal case: every failure here means one badge renders
/// without a figure, so the worker logs and waits for the next tick.
pub fn spawn(
    chain_id: i64,
    pool: DbPool,
    rpc: &RpcEndpoint,
    cache: VenueApyCache,
    assets: Arc<AssetRegistry>,
) {
    let mut worker = VenueApyWorker::new(chain_id, pool, rpc, cache);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REFRESH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match assets.for_chain(chain_id).await {
                Ok(rows) => worker.refresh(&rows).await,
                Err(e) => warn!(chain_id, error = %e, "venue apy: asset read failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A share price of `1.0 + pct/100`, in a vault whose probe reads `1e18`.
    fn price(pct: f64) -> U256 {
        U256::from((1e18 * (1.0 + pct / 100.0)) as u128)
    }

    const DAY: i64 = 24 * 60 * 60;

    #[test]
    fn compounds_a_window_up_to_a_year() {
        // 1% over a quarter is four such windows: 1.01^4 - 1 = 4.06%.
        let bps = annualize_bps(price(1.0), price(0.0), 365 * DAY / 4).unwrap();
        assert_eq!(bps, 406);
    }

    #[test]
    fn is_the_growth_itself_over_exactly_a_year() {
        let bps = annualize_bps(price(5.0), price(0.0), 365 * DAY).unwrap();
        assert_eq!(bps, 500);
    }

    /// The exponent is `year / window`, so a short window multiplies whatever it
    /// caught. An hour of drift is not a rate and is not reported as one.
    #[test]
    fn refuses_a_window_under_the_floor() {
        assert!(annualize_bps(price(0.02), price(0.0), MIN_WINDOW_SECONDS - 1).is_none());
        assert!(annualize_bps(price(0.02), price(0.0), MIN_WINDOW_SECONDS).is_some());
    }

    #[test]
    fn reports_a_venue_loss_rather_than_clamping_it() {
        let bps = annualize_bps(price(0.0), price(5.0), 365 * DAY).unwrap();
        assert!(bps < 0, "{bps}");
    }

    /// A vault reindexed inside the window leaves two readings with no common
    /// basis. The ratio is arithmetic; the rate it implies is fiction.
    #[test]
    fn drops_a_reading_too_wild_to_be_a_rate() {
        let wild = U256::from(10_000u64) * price(0.0);
        assert!(annualize_bps(wild, price(0.0), 30 * DAY).is_none());
    }

    #[test]
    fn has_no_answer_without_both_readings() {
        assert!(annualize_bps(price(1.0), U256::ZERO, 30 * DAY).is_none());
        assert!(annualize_bps(U256::ZERO, price(1.0), 30 * DAY).is_none());
    }

    /// Both readings can exceed `u128`, so the division has to happen before
    /// anything becomes a float.
    #[test]
    fn keeps_precision_on_large_readings() {
        let then = U256::from(10u64).pow(U256::from(40u64));
        let now = then * U256::from(101u64) / U256::from(100u64);
        assert_eq!(annualize_bps(now, then, 365 * DAY).unwrap(), 100);
    }

    #[test]
    fn nets_out_what_the_pool_keeps() {
        // 10% of the yield to the treasury, a fifth of custody held idle.
        assert_eq!(net_of_pool(500, 1_000, 2_000), 360);
        // Nothing kept, nothing idle: the vault's rate reaches the holder whole.
        assert_eq!(net_of_pool(500, 0, 0), 500);
        // A loss is damped by the buffer too — the idle fraction did not lose
        // either. Reporting the vault's full loss would be the wrong direction.
        assert_eq!(net_of_pool(-500, 0, 2_000), -400);
    }

    #[test]
    fn converts_the_window_into_blocks_at_the_measured_block_time() {
        // 2s blocks: a week is 302,400 of them.
        assert_eq!(
            window_start_block(1_000_000, 10_000, 995_000, 0),
            Some(697_600)
        );
        // 12s blocks: a sixth as many.
        assert_eq!(
            window_start_block(1_000_000, 60_000, 995_000, 0),
            Some(949_600)
        );
    }

    #[test]
    fn has_no_window_on_a_chain_younger_than_one() {
        // 12s blocks, 1,000 blocks of history: a week ago is before genesis.
        assert_eq!(window_start_block(1_000, 12_000, 0, 0), None);
    }

    /// Both are shapes a node can return: a reorg-adjacent head, or timestamps
    /// that did not advance between two blocks.
    #[test]
    fn has_no_window_from_a_probe_that_says_nothing() {
        assert_eq!(window_start_block(1_000_000, 0, 995_000, 0), None);
        assert_eq!(window_start_block(100, 1_000, 100, 1_000), None);
    }
}
