// Gas + EIP-1559 fee estimator. Builds an alloy provider per call
// (mirrors `submitter.rs` v1 pattern; persistent provider TBD).
//
// Picks EIP-1559 path when the latest block exposes `baseFeePerGas`; falls
// back to legacy `eth_gasPrice` otherwise (BSC, some sidechains).
//
// Caveat: on optimistic rollups (Arbitrum, Optimism) `eth_estimateGas`
// returns L2 execution gas only; L1 data-availability fee is separate
// and can dominate. v1 documents this as a known undercount.

use crate::domain::error::{AppError, AppResult};
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{BlockNumberOrTag, TransactionRequest};
use std::str::FromStr;

pub struct GasEstimator {
    pub chain_id: i64,
    pub rpc_url: String,
    pub from: Address,
}

#[derive(Debug, Clone, Copy)]
pub struct GasQuote {
    pub gas_used: u64,
    pub base_fee_per_gas_wei: u128,
    pub max_priority_fee_per_gas_wei: u128,
    pub effective_gas_price_wei: u128,
}

impl GasEstimator {
    pub fn new(chain_id: i64, rpc_url: &str, from: Address) -> Self {
        Self {
            chain_id,
            rpc_url: rpc_url.to_string(),
            from,
        }
    }

    pub async fn quote(&self, to: Address, calldata: Vec<u8>) -> AppResult<GasQuote> {
        let url: alloy::transports::http::reqwest::Url = self
            .rpc_url
            .parse()
            .map_err(|e: url::ParseError| AppError::Internal(format!("rpc url: {e}")))?;
        let provider = ProviderBuilder::new().on_http(url);

        let tx = TransactionRequest::default()
            .from(self.from)
            .to(to)
            .input(Bytes::from(calldata).into());

        let gas_used = provider
            .estimate_gas(&tx)
            .await
            .map_err(|e| AppError::Rpc(format!("eth_estimateGas: {e}")))?;

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

        Ok(GasQuote {
            gas_used,
            base_fee_per_gas_wei: base_fee,
            max_priority_fee_per_gas_wei: priority_fee,
            effective_gas_price_wei: effective,
        })
    }
}

/// Helper for handlers that read an `Address` from a hex string.
pub fn parse_addr(hex: &str) -> AppResult<Address> {
    Address::from_str(hex).map_err(|e| AppError::Internal(format!("address parse: {e}")))
}

/// Total native wei = gas_used * effective_price * (10_000 + markup_bps) / 10_000
pub fn apply_markup(gas_used: u64, effective_gas_price_wei: u128, markup_bps: u32) -> U256 {
    let raw = U256::from(gas_used) * U256::from(effective_gas_price_wei);
    raw * U256::from(10_000u32 + markup_bps) / U256::from(10_000u32)
}
