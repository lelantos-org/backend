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
use alloy::transports::http::reqwest::Url;
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use std::sync::Arc;

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

#[async_trait]
pub trait MaspYieldReader: Send + Sync {
    /// The pool address is read from the venue rather than configured.
    ///
    /// `ERC4626Venue` is pinned to one pool at construction and exposes it as an
    /// immutable, and `MASP.addYieldAsset` refuses a venue pinned anywhere else.
    /// Taking it from there rather than from `ChainCfg` keeps the indexer's
    /// configuration unchanged and makes the pairing impossible to misconfigure.
    async fn yield_state(
        &self,
        venue: Address,
        asset_id: u64,
    ) -> Result<YieldState, ExplorerIndexerError>;
}

pub type DynMaspYieldReader = Arc<dyn MaspYieldReader>;

pub struct HttpMaspYieldReader {
    inner: RootProvider<Http<Client>>,
}

impl HttpMaspYieldReader {
    pub fn build(rpc_url: &str) -> Result<Arc<Self>, ExplorerIndexerError> {
        let url: Url = rpc_url
            .parse()
            .map_err(|e| ExplorerIndexerError::Config(format!("rpc_url: {e}")))?;
        Ok(Arc::new(Self {
            inner: ProviderBuilder::new().on_http(url),
        }))
    }
}

#[async_trait]
impl MaspYieldReader for HttpMaspYieldReader {
    async fn yield_state(
        &self,
        venue: Address,
        asset_id: u64,
    ) -> Result<YieldState, ExplorerIndexerError> {
        let venue_contract = IYieldVenue::new(venue, self.inner.clone());

        let masp = venue_contract
            .POOL()
            .call()
            .await
            .map(|v| v._0)
            .map_err(|e| ExplorerIndexerError::Rpc(format!("{venue}.POOL(): {e}")))?;

        let block_number = self
            .inner
            .get_block_number()
            .await
            .map_err(|e| ExplorerIndexerError::Rpc(format!("get_block_number(): {e}")))?;

        let r = IMaspYield::new(masp, self.inner.clone())
            .yieldState(asset_id)
            .call()
            .await
            .map_err(|e| {
                ExplorerIndexerError::Rpc(format!("{masp}.yieldState({asset_id}): {e}"))
            })?;

        let venue_assets = venue_contract
            .totalAssets()
            .call()
            .await
            .map(|v| v._0)
            .map_err(|e| ExplorerIndexerError::Rpc(format!("{venue}.totalAssets(): {e}")))?;

        Ok(YieldState {
            total_normalized: r.totalNormalized,
            accrued_fee_normalized: r.accruedFeeNormalized,
            idle: r.idle,
            last_idx: r.lastIdx,
            index_ray: r.index,
            gross: venue_assets + r.idle,
            block_number,
        })
    }
}
