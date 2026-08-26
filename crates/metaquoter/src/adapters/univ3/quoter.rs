use crate::adapters::univ3::abi::{FEE_TIERS, IQuoterV2};
use crate::domain::error::AppError;
use crate::domain::models::{Quote, QuoteRequest, Venue};
use crate::domain::time::now_secs;
use crate::repositories::quoter::Quoter;
use alloy::primitives::aliases::{U24, U160};
use alloy::primitives::{Address, U256};
use alloy::providers::RootProvider;
use alloy::sol_types::SolValue;
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use std::collections::HashMap;

/// Wrapper overhead added on top of the venue's reported `gasEstimate`: two
/// MASP transacts (~250k each, inline verifier) plus ~85k wrapper bookkeeping.
const WRAPPER_OVERHEAD_GAS: u64 = 585_000;

/// Per-chain config wired by `main.rs`.
pub struct ChainSetup {
    pub provider: RootProvider<Http<Client>>,
    pub quoter_addr: Address,
    pub adapter_addr: Address,
    /// MASP fee bps deducted from the venue's gross output before slippage.
    pub masp_fee_bps: u16,
}

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
        // MASP charges its fee on top of `expected_out` (see
        // `MASP._computeAmounts`), so `expected_out` is the reciprocal of the
        // fee applied to gross rather than gross minus fee.
        let expected_out = max_deposit(best.amount_out, setup.masp_fee_bps);
        let masp_fee = best.amount_out.saturating_sub(expected_out);

        Ok(Quote {
            venue: Venue::UniV3,
            adapter: setup.adapter_addr,
            // Single-hop route layout: abi.encode(uint24 fee, uint160
            // sqrtPriceLimitX96). Zero disables the pool's slippage guard;
            // `min_out` provides sandwich protection at this layer.
            route: (U24::from(best.fee), U160::ZERO).abi_encode().into(),
            min_out: req.apply_slippage(expected_out),
            expected_out,
            gas_estimate: best.gas_estimate.saturating_add(WRAPPER_OVERHEAD_GAS),
            quoted_at: now_secs(),
            masp_fee,
            masp_fee_bps: setup.masp_fee_bps,
        })
    }
}

/// Largest `expected_out` such that
/// `expected_out + expected_out * bps / 10_000 <= gross`, equivalent to
/// `gross * 10_000 / (10_000 + bps)`. `bps` is clamped to 10_000.
fn max_deposit(gross: U256, bps: u16) -> U256 {
    let bps = u32::from(bps).min(10_000);
    let denom = U256::from(10_000u32 + bps);
    gross.saturating_mul(U256::from(10_000u32)) / denom
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

    #[test]
    fn max_deposit_reciprocal_fee() {
        assert_eq!(
            max_deposit(U256::from(1_000_000u64), 0),
            U256::from(1_000_000u64)
        );

        // 50 bps fee: expected_out = gross * 10_000 / 10_050 ≈ 995_024, so
        // masp_fee = gross - expected_out ≈ 4_975 ≈ expected_out * 50/10_000.
        let gross = U256::from(1_000_000u64);
        let expected = max_deposit(gross, 50);
        let fee = gross - expected;
        assert_eq!(expected, U256::from(995_024u64));
        // Fee on expected_out, rounded down to match integer arithmetic.
        assert_eq!(
            fee,
            expected * U256::from(50u32) / U256::from(10_000u32) + U256::from(1u8)
        );

        assert_eq!(max_deposit(gross, u16::MAX), max_deposit(gross, 10_000));
    }
}
