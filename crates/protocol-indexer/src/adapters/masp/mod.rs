//! Yield-index reads from the pool.
//!
//! The index is not in any log. `PerfFeeAccrued` carries the fee mark and
//! `Rebalanced` the idle split, but the quantity every conversion needs —
//! `gross / supply` — moves with the venue's own accounting on every block, and
//! no event fires when it does. Only the chain can answer it.
//!
//! A round batches through Multicall3 where the chain has it (`multicall`) and
//! falls back to two reads per asset where it does not (`per_asset`).

mod multicall;
mod per_asset;

use crate::domain::error::ProtocolIndexerError;
use alloy::primitives::{Address, U256};
use alloy::providers::{ProviderBuilder, RootProvider};
use alloy::sol;
use async_trait::async_trait;
use chain_types::abi::IYieldVenue;
use chain_types::rpc::{HttpTransport, RpcEndpoint, RpcTimeouts};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock};
use tracing::warn;

sol! {
    #[sol(rpc)]
    interface IMaspYield {
        function yieldState(uint64 id) external view returns (
            address venue,
            uint16 bufferBps,
            uint16 perfBps,
            bool halted,
            uint256 totalNormalized,
            uint256 accruedFeeNormalized,
            uint256 idle,
            uint256 lastIdx,
            uint256 index
        );
    }
}

/// One asset's index state, as of `block_number`.
#[derive(Debug, Clone)]
pub struct YieldState {
    pub total_normalized: U256,
    pub accrued_fee_normalized: U256,
    pub idle: U256,
    pub last_idx: U256,
    pub index_ray: U256,
    /// Venue position plus idle — the numerator every conversion divides
    /// `supply` into.
    ///
    /// Read as `venue.totalAssets() + idle`, which is how the contract itself
    /// computes it, rather than recovered from the reported index. The index is
    /// `gross * RAY / (supply * scale)`, so inverting it would divide and then
    /// multiply by a rounded value and land a unit or two away from what the
    /// pool would actually pay.
    pub gross: U256,
    pub block_number: u64,
}

/// One asset's reads, resolved enough to issue.
///
/// A named struct rather than `(Address, Address, u64)`: the two addresses are
/// positionally interchangeable, and `round_batched` targets one for
/// `yieldState` and the other for `totalAssets` — swapping them would compile
/// and read the wrong contract.
#[derive(Debug, Clone, Copy)]
struct AssetRead {
    venue: Address,
    pool: Address,
    id: u64,
}

#[async_trait]
pub trait MaspYieldReader: Send + Sync {
    /// Refresh every `(venue, asset_id)` in one round.
    ///
    /// A round rather than an asset: the reads are independent and share a
    /// block, so batching them is both cheaper and more honest than sampling a
    /// height per asset and hoping none of them straddles a block.
    ///
    /// One entry out per entry in, in order. `None` is that asset's reads
    /// failing, always logged where it happens — one unreachable venue must not
    /// discard the rest of the round, and every stage here upholds that: the
    /// pool lookup drops the asset it belongs to, and `aggregate3` isolates the
    /// rest through `allowFailure`.
    ///
    /// The pool address is read from the venue rather than configured.
    /// `ERC4626Venue` is pinned to one pool at construction and exposes it as an
    /// immutable, and `MASP.addYieldAsset` refuses a venue pinned anywhere else.
    /// Taking it from there rather than from `ChainCfg` keeps the indexer's
    /// configuration unchanged and makes the pairing impossible to misconfigure.
    async fn round(
        &self,
        assets: &[(Address, u64)],
    ) -> Result<Vec<Option<YieldState>>, ProtocolIndexerError>;
}

pub type DynMaspYieldReader = Arc<dyn MaspYieldReader>;

pub struct HttpMaspYieldReader {
    inner: RootProvider<HttpTransport>,
    /// `venue -> POOL()`, resolved once per venue for the life of the process.
    ///
    /// `POOL` is an immutable on `ERC4626Venue`, so re-reading it every refresh
    /// bought nothing and cost a third of this service's `eth_call` traffic: one
    /// call per asset per round, forever, for a value that cannot change. A
    /// venue is only ever rebound by a redeploy, which restarts this process.
    pools: RwLock<HashMap<Address, Address>>,
    /// Whether this chain answers at [`MULTICALL3`](chain_types::abi::MULTICALL3), probed once.
    ///
    /// A capability question — "is there code here" — distinct from the address
    /// itself, which is a chain fact and comes from config. `OnceCell` leaves
    /// itself unset when the probe errors, so a transport blip costs one slow
    /// round rather than pinning the reader to the fallback for the life of the
    /// process.
    ///
    /// A cached `true` can still go stale — a dev chain reset out from under a
    /// running indexer leaves no code at the address — so `round` falls back for
    /// any round whose batch fails rather than trusting this alone.
    multicall: OnceCell<bool>,
}

/// Deadline for one yield-state read.
///
/// Wider than the metadata sweep's: a `yieldState` round batches every asset
/// through Multicall3, so one call carries the whole chain's work. Bounded at
/// all because the read runs inside a tick and would otherwise park it.
const TIMEOUTS: RpcTimeouts = RpcTimeouts::request(20);

impl HttpMaspYieldReader {
    pub fn build(rpc_url: &str) -> Result<Arc<Self>, ProtocolIndexerError> {
        let rpc = RpcEndpoint::new(rpc_url, TIMEOUTS)
            .map_err(|e| ProtocolIndexerError::Config(format!("rpc_url: {e}")))?;
        Ok(Arc::new(Self {
            inner: ProviderBuilder::new().on_client(rpc.client()),
            pools: RwLock::new(HashMap::new()),
            multicall: OnceCell::new(),
        }))
    }

    /// The pool a venue is pinned to, from the cache or from the chain.
    ///
    /// A concurrent miss on the same venue reads twice and writes the same
    /// value, which is why this takes no lock across the await: holding one
    /// would serialise every venue behind the slowest RPC call to save a
    /// duplicate read that happens once per process.
    async fn pool_of(&self, venue: Address) -> Result<Address, ProtocolIndexerError> {
        if let Some(masp) = self.pools.read().await.get(&venue) {
            return Ok(*masp);
        }
        let masp = IYieldVenue::new(venue, self.inner.clone())
            .POOL()
            .call()
            .await
            .map(|v| v._0)
            .map_err(|e| ProtocolIndexerError::Rpc(format!("{venue}.POOL(): {e}")))?;
        self.pools.write().await.insert(venue, masp);
        Ok(masp)
    }

    /// Assemble one asset's state. Spelled once so the batched and per-asset
    /// paths cannot drift — dev takes the fallback and every chain with
    /// Multicall3 takes the batch, so a divergence would surface only in
    /// production.
    fn state_of(
        r: IMaspYield::yieldStateReturn,
        venue_assets: U256,
        block_number: u64,
    ) -> YieldState {
        YieldState {
            total_normalized: r.totalNormalized,
            accrued_fee_normalized: r.accruedFeeNormalized,
            idle: r.idle,
            last_idx: r.lastIdx,
            index_ray: r.index,
            // Saturating, not wrapping: ruint's `+` wraps silently in release, so
            // a venue reporting `u256::MAX` would write a tiny `gross` and make
            // every conversion downstream wrong with no signal.
            gross: venue_assets.saturating_add(r.idle),
            block_number,
        }
    }
}

#[async_trait]
impl MaspYieldReader for HttpMaspYieldReader {
    async fn round(
        &self,
        assets: &[(Address, u64)],
    ) -> Result<Vec<Option<YieldState>>, ProtocolIndexerError> {
        // Pools first, cached after the first round. Resolved per asset rather
        // than with `?`: a venue that is an EOA or has no `POOL()` would
        // otherwise abort the round and strand every other asset on the chain
        // forever, which is exactly what this trait promises not to do.
        let mut reads = Vec::with_capacity(assets.len());
        let mut slots = Vec::with_capacity(assets.len());
        for (venue, id) in assets {
            match self.pool_of(*venue).await {
                Ok(pool) => {
                    slots.push(Some(reads.len()));
                    reads.push(AssetRead {
                        venue: *venue,
                        pool,
                        id: *id,
                    });
                }
                Err(e) => {
                    warn!(asset_id = id, venue = %venue, error = %e, "venue POOL() failed");
                    slots.push(None);
                }
            }
        }

        let mut states = if self.has_multicall().await {
            match self.round_batched(&reads).await {
                Ok(states) => states,
                // A cached `true` that has gone stale — a chain reset under a
                // running indexer — reads as a batch failure. Falling back keeps
                // the round correct at the cost of one wasted call.
                Err(e) => {
                    warn!(error = %e, "multicall round failed; falling back to per-asset reads");
                    self.round_per_asset(&reads).await?
                }
            }
        } else {
            self.round_per_asset(&reads).await?
        }
        .into_iter();

        // Re-expand to one entry per requested asset, so the caller's `zip`
        // lines up even when a pool lookup dropped one.
        Ok(slots
            .into_iter()
            .map(|slot| slot.and_then(|_| states.next().flatten()))
            .collect())
    }
}
