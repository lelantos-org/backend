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

#[derive(Debug, Clone, Copy)]
pub struct FeeData {
    pub base_fee_per_gas_wei: u128,
    pub max_priority_fee_per_gas_wei: u128,
    pub effective_gas_price_wei: u128,
}

impl GasEstimator {
    pub fn new(chain_id: i64, rpc: RpcEndpoint) -> Self {
        Self { chain_id, rpc }
    }

    pub async fn fee_data(&self) -> AppResult<FeeData> {
        let provider = ProviderBuilder::new().on_client(self.rpc.client());

        let block = provider
            .get_block_by_number(BlockNumberOrTag::Latest, false)
            .await
            .map_err(|e| AppError::Rpc(format!("eth_getBlockByNumber: {e}")))?
            .ok_or_else(|| AppError::Rpc("latest block missing".into()))?;

        let base_fee_opt: Option<u128> = block.header.base_fee_per_gas.map(|v| v as u128);

        let (base_fee, priority_fee, effective) = if let Some(base) = base_fee_opt {
            let priority = provider
                .get_max_priority_fee_per_gas()
                .await
                .map_err(|e| AppError::Rpc(format!("eth_maxPriorityFeePerGas: {e}")))?;
            (base, priority, base.saturating_add(priority))
        } else {
            // Legacy chain: `eth_gasPrice` is the effective price, treated as the
            // full base.
            let gp = provider
                .get_gas_price()
                .await
                .map_err(|e| AppError::Rpc(format!("eth_gasPrice: {e}")))?;
            (gp, 0u128, gp)
        };

        Ok(FeeData {
            base_fee_per_gas_wei: base_fee,
            max_priority_fee_per_gas_wei: priority_fee,
            effective_gas_price_wei: effective,
        })
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
