//! Yield-index reads from the pool.
//!
//! The index is not in any log. `PerfFeeAccrued` carries the fee mark and
//! `Rebalanced` the idle split, but the quantity every conversion needs —
//! `gross / supply` — moves with the venue's own accounting on every block, and
//! no event fires when it does. Only the chain can answer it.

use crate::error::ExplorerIndexerError;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::http::reqwest::Url;
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use chain_types::abi::{IMulticall3, MULTICALL3};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock};
use tracing::{info, warn};

sol! {
    #[sol(rpc)]
    interface IYieldVenue {
        function totalAssets() external view returns (uint256);
        function POOL() external view returns (address);
    }

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
    ) -> Result<Vec<Option<YieldState>>, ExplorerIndexerError>;
}

pub type DynMaspYieldReader = Arc<dyn MaspYieldReader>;

pub struct HttpMaspYieldReader {
    inner: RootProvider<Http<Client>>,
    /// `venue -> POOL()`, resolved once per venue for the life of the process.
    ///
    /// `POOL` is an immutable on `ERC4626Venue`, so re-reading it every refresh
    /// bought nothing and cost a third of this service's `eth_call` traffic: one
    /// call per asset per round, forever, for a value that cannot change. A
    /// venue is only ever rebound by a redeploy, which restarts this process.
    pools: RwLock<HashMap<Address, Address>>,
    /// Whether this chain answers at [`MULTICALL3`], probed once.
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

impl HttpMaspYieldReader {
    pub fn build(rpc_url: &str) -> Result<Arc<Self>, ExplorerIndexerError> {
        let url: Url = rpc_url
            .parse()
            .map_err(|e| ExplorerIndexerError::Config(format!("rpc_url: {e}")))?;
        Ok(Arc::new(Self {
            inner: ProviderBuilder::new().on_http(url),
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
    async fn pool_of(&self, venue: Address) -> Result<Address, ExplorerIndexerError> {
        if let Some(masp) = self.pools.read().await.get(&venue) {
            return Ok(*masp);
        }
        let masp = IYieldVenue::new(venue, self.inner.clone())
            .POOL()
            .call()
            .await
            .map(|v| v._0)
            .map_err(|e| ExplorerIndexerError::Rpc(format!("{venue}.POOL(): {e}")))?;
        self.pools.write().await.insert(venue, masp);
        Ok(masp)
    }
}

impl HttpMaspYieldReader {
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

    /// Whether this chain carries Multicall3, probed once.
    async fn has_multicall(&self) -> bool {
        *self
            .multicall
            .get_or_try_init(|| async {
                let present = !self.inner.get_code_at(MULTICALL3).await?.is_empty();
                // Both outcomes, once: this is the fact that explains the
                // service's whole request count, and inferring it from a node's
                // logs is not something anyone should have to do.
                if present {
                    info!(address = %MULTICALL3, "yield rounds batch through Multicall3");
                } else {
                    info!(address = %MULTICALL3, "no Multicall3; yield rounds read per asset");
                }
                Ok::<bool, alloy::transports::RpcError<alloy::transports::TransportErrorKind>>(
                    present,
                )
            })
            .await
            .inspect_err(|e| warn!(error = %e, "Multicall3 probe failed; reading per asset"))
            .unwrap_or(&false)
    }

    /// One `eth_call`: the head plus every asset's two reads.
    ///
    /// `allowFailure` per call, so a venue that reverts costs its own asset and
    /// nothing else — the same isolation the per-asset path gets from handling
    /// each `Result` separately.
    async fn round_batched(
        &self,
        reads: &[AssetRead],
    ) -> Result<Vec<Option<YieldState>>, ExplorerIndexerError> {
        let mut calls = Vec::with_capacity(reads.len() * 2 + 1);
        calls.push(IMulticall3::Call3 {
            target: MULTICALL3,
            allowFailure: false,
            callData: IMulticall3::getBlockNumberCall {}.abi_encode().into(),
        });
        for r in reads {
            calls.push(IMulticall3::Call3 {
                target: r.pool,
                allowFailure: true,
                callData: IMaspYield::yieldStateCall { id: r.id }.abi_encode().into(),
            });
            calls.push(IMulticall3::Call3 {
                target: r.venue,
                allowFailure: true,
                callData: IYieldVenue::totalAssetsCall {}.abi_encode().into(),
            });
        }

        let out = IMulticall3::new(MULTICALL3, self.inner.clone())
            .aggregate3(calls)
            .call()
            .await
            .map_err(|e| ExplorerIndexerError::Rpc(format!("multicall3.aggregate3: {e}")))?
            .returnData;

        // `allowFailure: false` on the head, so Multicall3 reverts the batch
        // rather than returning it unset — a missing head here means the call
        // failed wholesale, not that one asset did.
        let head = out
            .first()
            .filter(|r| r.success)
            .ok_or_else(|| ExplorerIndexerError::Rpc("multicall3: no block number".into()))?;
        let block_number: u64 =
            IMulticall3::getBlockNumberCall::abi_decode_returns(&head.returnData, false)
                .map_err(|e| ExplorerIndexerError::Rpc(format!("multicall3.getBlockNumber: {e}")))?
                .blockNumber
                .try_into()
                // `U256::to` panics rather than truncating, and a panic here takes
                // the whole poller down silently.
                .map_err(|_| ExplorerIndexerError::Rpc("block number exceeds u64".into()))?;

        // Paired off the tail rather than indexed with `1 + i * 2`: the layout is
        // then stated once, where the calls are pushed, instead of twice.
        let mut pairs = out.get(1..).unwrap_or_default().chunks_exact(2);
        let states = reads
            .iter()
            .map(|read| {
                let [state, assets] = pairs.next()? else {
                    warn!(
                        asset_id = read.id,
                        "multicall3 returned no result for this asset"
                    );
                    return None;
                };
                if !state.success || !assets.success {
                    // Logged here because the per-asset path logs its failures
                    // too; silence on one path only would make a permanently
                    // stale row invisible on whichever chain took it.
                    warn!(
                        asset_id = read.id,
                        venue = %read.venue,
                        yield_state_ok = state.success,
                        total_assets_ok = assets.success,
                        "yield read reverted"
                    );
                    return None;
                }
                let r = IMaspYield::yieldStateCall::abi_decode_returns(&state.returnData, false)
                    .inspect_err(
                        |e| warn!(asset_id = read.id, error = %e, "yieldState decode failed"),
                    )
                    .ok()?;
                let venue_assets =
                    IYieldVenue::totalAssetsCall::abi_decode_returns(&assets.returnData, false)
                        .inspect_err(
                            |e| warn!(venue = %read.venue, error = %e, "totalAssets decode failed"),
                        )
                        .ok()?
                        ._0;
                Some(Self::state_of(r, venue_assets, block_number))
            })
            .collect();

        Ok(states)
    }

    /// Two reads per asset plus one for the head, for a chain without
    /// Multicall3. Each asset's pair is joined; the assets run concurrently.
    async fn round_per_asset(
        &self,
        reads: &[AssetRead],
    ) -> Result<Vec<Option<YieldState>>, ExplorerIndexerError> {
        let block_number = self
            .inner
            .get_block_number()
            .await
            .map_err(|e| ExplorerIndexerError::Rpc(format!("get_block_number(): {e}")))?;

        Ok(
            futures::future::join_all(reads.iter().map(|read| async move {
                match self.one(*read, block_number).await {
                    Ok(state) => Some(state),
                    Err(e) => {
                        warn!(asset_id = read.id, error = %e, "yield state read failed");
                        None
                    }
                }
            }))
            .await,
        )
    }

    async fn one(
        &self,
        read: AssetRead,
        block_number: u64,
    ) -> Result<YieldState, ExplorerIndexerError> {
        let (r, venue_assets) = tokio::try_join!(
            async {
                IMaspYield::new(read.pool, self.inner.clone())
                    .yieldState(read.id)
                    .call()
                    .await
                    .map_err(|e| {
                        ExplorerIndexerError::Rpc(format!(
                            "{}.yieldState({}): {e}",
                            read.pool, read.id
                        ))
                    })
            },
            async {
                IYieldVenue::new(read.venue, self.inner.clone())
                    .totalAssets()
                    .call()
                    .await
                    .map(|v| v._0)
                    .map_err(|e| {
                        ExplorerIndexerError::Rpc(format!("{}.totalAssets(): {e}", read.venue))
                    })
            },
        )?;

        Ok(Self::state_of(r, venue_assets, block_number))
    }
}

#[async_trait]
impl MaspYieldReader for HttpMaspYieldReader {
    async fn round(
        &self,
        assets: &[(Address, u64)],
    ) -> Result<Vec<Option<YieldState>>, ExplorerIndexerError> {
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
