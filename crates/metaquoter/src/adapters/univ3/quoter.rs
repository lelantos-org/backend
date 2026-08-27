use crate::adapters::chain_setup::ChainSetup;
use crate::adapters::univ3::abi::{FEE_TIERS, IQuoterV2};
use crate::domain::error::AppError;
use crate::domain::models::{Quote, QuoteRequest, Venue};
use crate::repositories::quoter::Quoter;
use alloy::primitives::U256;
use alloy::primitives::aliases::{U24, U160};
use alloy::sol_types::SolValue;
use async_trait::async_trait;
use std::collections::HashMap;

pub struct UniV3Quoter {
    chains: HashMap<u64, ChainSetup>,
}

impl UniV3Quoter {
    pub fn new(chains: HashMap<u64, ChainSetup>) -> Self {
        Self { chains }
    }
}

#[async_trait]
impl Quoter for UniV3Quoter {
    fn venue(&self) -> Venue {
        Venue::UniV3
    }

    fn supports_chain(&self, chain_id: u64) -> bool {
        self.chains.contains_key(&chain_id)
    }

    async fn quote(&self, req: &QuoteRequest) -> Result<Quote, AppError> {
        let setup = self
            .chains
            .get(&req.chain_id)
            .ok_or(AppError::UnsupportedChain(req.chain_id))?;

        let best = best_tier(setup, req).await?;
        // Single-hop route layout: abi.encode(uint24 fee, uint160
        // sqrtPriceLimitX96). Zero disables the pool's slippage guard;
        // `min_out` provides sandwich protection at this layer.
        let route = (U24::from(best.fee), U160::ZERO).abi_encode().into();

        Ok(setup.build_quote(Venue::UniV3, req, best.amount_out, best.gas_estimate, route))
    }
}

struct TierQuote {
    fee: u32,
    amount_out: U256,
    gas_estimate: u64,
}

/// Race all canonical fee tiers and pick the highest-output pool. Tiers without
/// a deployed pool revert at the quoter and are dropped. Returns
/// [`AppError::NoLiquidity`] if every tier fails.
async fn best_tier(setup: &ChainSetup, req: &QuoteRequest) -> Result<TierQuote, AppError> {
    let quoter = IQuoterV2::new(setup.quoter_addr, &setup.provider);

    let calls = FEE_TIERS.iter().map(|&fee| {
        let params = IQuoterV2::QuoteExactInputSingleParams {
            tokenIn: req.token_in,
            tokenOut: req.token_out,
            amountIn: req.amount_in,
            fee: U24::from(fee),
            sqrtPriceLimitX96: U160::ZERO,
        };
        let call = quoter.quoteExactInputSingle(params);
        async move {
            let r = call.call().await.ok()?;
            Some(TierQuote {
                fee,
                amount_out: r.amountOut,
                gas_estimate: r.gasEstimate.try_into().unwrap_or(u64::MAX),
            })
        }
    });

    futures::future::join_all(calls)
        .await
        .into_iter()
        .flatten()
        .max_by_key(|t| t.amount_out)
        .ok_or(AppError::NoLiquidity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

    fn req(slippage_bps: u16) -> QuoteRequest {
        QuoteRequest {
            chain_id: 1,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            amount_in: U256::from(1_000_000u64),
            slippage_bps,
        }
    }

    #[test]
    fn slippage_math_basis_points() {
        let expected = U256::from(1_000_000u64);
        assert_eq!(req(50).apply_slippage(expected), U256::from(995_000u64));
        assert_eq!(req(0).apply_slippage(expected), expected);
        assert_eq!(req(10_000).apply_slippage(expected), U256::ZERO);
    }
}
