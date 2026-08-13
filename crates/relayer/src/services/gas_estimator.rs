// EIP-1559 fee-data source for quotes.
//
// Picks EIP-1559 path when the latest block exposes `baseFeePerGas`; falls
// back to legacy `eth_gasPrice` otherwise (BSC, some sidechains).
//
// Gas *units* come from `gas_witness`, not from `eth_estimateGas` — see that
// module for why. This type only answers "what does a unit of gas cost right
// now".
//
// Caveat: on optimistic rollups (Arbitrum, Optimism) execution gas excludes
// the L1 data-availability fee, which can dominate. v1 documents this as a
// known undercount.

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
            // Legacy chain: use eth_gasPrice as effective; treat as full base.
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

/// Total native wei = gas_used * effective_price * (10_000 + markup_bps) / 10_000
pub fn apply_markup(gas_used: u64, effective_gas_price_wei: u128, markup_bps: u32) -> U256 {
    let raw = U256::from(gas_used) * U256::from(effective_gas_price_wei);
    raw * U256::from(10_000u32 + markup_bps) / U256::from(10_000u32)
}
