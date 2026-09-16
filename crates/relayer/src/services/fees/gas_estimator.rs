//! EIP-1559 fee-data source for quotes.
//!
//! Takes the EIP-1559 path when the latest block exposes `baseFeePerGas` and
//! falls back to legacy `eth_gasPrice` otherwise, as on BSC and some sidechains.
//!
//! Gas units come from `gas_witness` rather than `eth_estimateGas`; see that
//! module. This type only answers what a unit of gas costs now.
//!
//! On optimistic rollups such as Arbitrum and Optimism, execution gas excludes
//! the L1 data-availability fee, which can dominate, so quotes there undercount.

use crate::adapters::rpc::RpcEndpoint;
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::BlockNumberOrTag;

pub struct GasEstimator {
    pub chain_id: i64,
    rpc: RpcEndpoint,
}

impl GasEstimator {
    pub fn new(chain_id: i64, rpc: RpcEndpoint) -> Self {
        Self { chain_id, rpc }
    }

    /// What one unit of gas costs now, in wei.
    pub async fn effective_gas_price_wei(&self) -> AppResult<u128> {
        let provider = ProviderBuilder::new().on_client(self.rpc.client());

        let block = provider
            .get_block_by_number(BlockNumberOrTag::Latest, false)
            .await
            .map_err(|e| AppError::Rpc(format!("eth_getBlockByNumber: {e}")))?
            .ok_or_else(|| AppError::Rpc("latest block missing".into()))?;

        match block.header.base_fee_per_gas {
            Some(base) => {
                let priority = provider
                    .get_max_priority_fee_per_gas()
                    .await
                    .map_err(|e| AppError::Rpc(format!("eth_maxPriorityFeePerGas: {e}")))?;
                Ok(u128::from(base).saturating_add(priority))
            }
            // Legacy chain: `eth_gasPrice` is the effective price.
            None => provider
                .get_gas_price()
                .await
                .map_err(|e| AppError::Rpc(format!("eth_gasPrice: {e}"))),
        }
    }
}

/// Total native wei:
/// `gas_used * effective_price * (10_000 + markup_bps) / 10_000`.
pub fn apply_markup(gas_used: u64, effective_gas_price_wei: u128, markup_bps: u32) -> U256 {
    let raw = U256::from(gas_used) * U256::from(effective_gas_price_wei);
    // Widened before the addition, since `10_000u32 + markup_bps` overflows for a
    // large configured markup. `RelayerConfig::validate` rejects those, but this
    // arithmetic does not depend on that.
    raw * (U256::from(10_000u32) + U256::from(markup_bps)) / U256::from(10_000u32)
}
