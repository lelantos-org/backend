// Orchestrates a fee estimate: observed gas units + live fee data + price
// oracle + configured fee tokens → final `EstimateResponse`.
//
// Each accepted fee token is priced concurrently; one that cannot be priced
// drops out of the quote rather than failing it.
// Amount math uses scaled integers (PRICE_SCALE=1e8) to avoid f64
// precision loss for low-decimal tokens vs 18-decimal native.

use crate::app::config::FeeTokenCfg;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{EstimateResponse, FeeQuote};
use crate::services::gas_estimator::{GasEstimator, apply_markup};
use crate::services::oracle::PriceOracle;
use alloy::primitives::{Address, U256};
use futures::future::join_all;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

const PRICE_SCALE: u128 = 100_000_000; // 1e8

pub struct FeeQuoter {
    pub chain_id: i64,
    pub native_symbol: String,
    pub native_decimals: u8,
    pub accepted_fee_tokens: Vec<FeeToken>,
    pub oracle: Arc<dyn PriceOracle>,
    pub gas_estimator: Arc<GasEstimator>,
    pub markup_bps: u32,
}

#[derive(Clone)]
pub struct FeeToken {
    pub symbol: String,
    pub address: Address,
    pub decimals: u8,
    pub quote_symbol: String,
}

impl FeeToken {
    pub fn from_cfg(c: &FeeTokenCfg) -> AppResult<Self> {
        let address = Address::from_str(&c.address)
            .map_err(|e| AppError::Internal(format!("fee_token {} address: {e}", c.symbol)))?;
        Ok(Self {
            symbol: c.symbol.clone(),
            address,
            decimals: c.decimals,
            quote_symbol: c.quote_symbol.clone(),
        })
    }
}

impl FeeQuoter {
    /// One token's share of the quote, or `None` if it could not be priced.
    fn quote_one(
        &self,
        token: &FeeToken,
        price: AppResult<f64>,
        total_native_wei: U256,
    ) -> Option<FeeQuote> {
        let price = match price {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    chain_id = self.chain_id,
                    token = token.symbol,
                    error = %e,
                    "fee token priced out of the quote"
                );
                return None;
            }
        };
        let amount = compute_token_amount(
            total_native_wei,
            self.native_decimals,
            token.decimals,
            price,
        );
        Some(FeeQuote {
            token_symbol: token.symbol.clone(),
            token_address: format!("{:#x}", token.address),
            decimals: token.decimals,
            amount: amount.to_string(),
        })
    }

    /// Price `gas_used` units at the chain's current fee data. Callers source
    /// the units from `gas_witness`, so this path costs one RPC round trip and
    /// a (usually cached) oracle lookup — no proving, no `eth_estimateGas`.
    pub async fn quote_for_gas(&self, gas_used: u64) -> AppResult<EstimateResponse> {
        let fee_data = self.gas_estimator.fee_data().await?;
        let total_native_wei =
            apply_markup(gas_used, fee_data.effective_gas_price_wei, self.markup_bps);

        let oracle = self.oracle.clone();
        let native = self.native_symbol.clone();
        let prices: Vec<AppResult<f64>> = join_all(self.accepted_fee_tokens.iter().map(|t| {
            let oracle = oracle.clone();
            let native = native.clone();
            let quote = t.quote_symbol.clone();
            async move { oracle.price(&native, &quote).await }
        }))
        .await;

        // One unresolvable pair drops that token from the quote rather than
        // failing the estimate: a caller who can pay in any of the others is
        // still served, and the boot-time check in `build_state` is what keeps
        // a permanently broken pair from going unnoticed.
        let fees: Vec<FeeQuote> = self
            .accepted_fee_tokens
            .iter()
            .zip(prices)
            .filter_map(|(token, price)| self.quote_one(token, price, total_native_wei))
            .collect();
        if fees.is_empty() && !self.accepted_fee_tokens.is_empty() {
            return Err(AppError::Oracle(format!(
                "chain {}: no accepted fee token could be priced",
                self.chain_id
            )));
        }

        let quoted_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Ok(EstimateResponse {
            gas_used,
            effective_gas_price_wei: fee_data.effective_gas_price_wei.to_string(),
            total_native_wei: total_native_wei.to_string(),
            markup_bps: self.markup_bps,
            quoted_at,
            fees,
        })
    }
}

/// `token_base = total_native_wei * price_scaled * 10^token_dec
///              / (10^native_dec * PRICE_SCALE)`
/// Scaled-integer math; `price` is folded into u128 once at the boundary.
pub fn compute_token_amount(
    total_native_wei: U256,
    native_dec: u8,
    token_dec: u8,
    price: f64,
) -> U256 {
    if !price.is_finite() || price <= 0.0 {
        return U256::ZERO;
    }
    let price_scaled = (price * PRICE_SCALE as f64).round() as u128;
    // `U256::pow` rather than `10u128.pow`, which panics past 38 decimals.
    // `RelayerConfig::validate` bounds both, but the arithmetic should not be
    // the thing that enforces it.
    let ten = U256::from(10u8);
    let num = total_native_wei * U256::from(price_scaled) * ten.pow(U256::from(token_dec));
    let den = ten.pow(U256::from(native_dec)) * U256::from(PRICE_SCALE);
    num / den
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_zero_keeps_raw_cost() {
        // gas=500_000, price=20 gwei. raw = 1e16 wei.
        let total = apply_markup(500_000, 20_000_000_000, 0);
        assert_eq!(total, U256::from(10_000_000_000_000_000u128));
    }

    #[test]
    fn markup_10pct_scales_correctly() {
        let total = apply_markup(500_000, 20_000_000_000, 1000);
        assert_eq!(total, U256::from(11_000_000_000_000_000u128));
    }

    #[test]
    fn compute_token_amount_eth_to_usdc() {
        // 0.011 ETH * 3000 USDC/ETH = 33 USDC (6 decimals = 33_000_000).
        let total_wei = U256::from(11_000_000_000_000_000u128); // 0.011 ETH
        let amt = compute_token_amount(total_wei, 18, 6, 3000.0);
        assert_eq!(amt, U256::from(33_000_000u128));
    }

    #[test]
    fn compute_token_amount_eth_to_weth_same_decimals() {
        let total_wei = U256::from(11_000_000_000_000_000u128);
        let amt = compute_token_amount(total_wei, 18, 18, 1.0);
        assert_eq!(amt, U256::from(11_000_000_000_000_000u128));
    }

    #[test]
    fn compute_token_amount_zero_price_safe() {
        let amt = compute_token_amount(U256::from(1u64), 18, 6, 0.0);
        assert_eq!(amt, U256::ZERO);
    }
}
