//! Uniswap V3 quote source: one `eth_call` to `QuoterV2` per canonical fee tier.

use crate::adapters::call;
use crate::adapters::chain_setup::{ChainMap, ChainSetup};
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
    chains: ChainMap,
}

impl UniV3Quoter {
    pub fn new(chains: HashMap<u64, ChainSetup>) -> Self {
        Self {
            chains: ChainMap::new(chains),
        }
    }
}

#[async_trait]
impl Quoter for UniV3Quoter {
    fn venue(&self) -> Venue {
        Venue::UniV3
    }

    fn supports_chain(&self, chain_id: u64) -> bool {
        self.chains.supports(chain_id)
    }

    async fn quote(&self, req: &QuoteRequest) -> Result<Quote, AppError> {
        let setup = self.chains.get(req.chain_id)?;

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

/// Race all canonical fee tiers and pick the highest-output pool. A tier with no
/// deployed pool reverts at the quoter and is dropped; see
/// [`call::best_tier`] for why a tier that fails any other way is not.
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
        let pending = quoter.quoteExactInputSingle(params);
        async move {
            pending.call().await.map(|r| TierQuote {
                fee,
                amount_out: r.amountOut,
                gas_estimate: call::gas_estimate(r.gasEstimate),
            })
        }
    });

    call::best_tier(Venue::UniV3, calls, |t: &TierQuote| t.amount_out).await
}
