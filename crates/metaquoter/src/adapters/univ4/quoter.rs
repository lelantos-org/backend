use crate::adapters::chain_setup::ChainSetup;
use crate::adapters::univ4::abi::{FEE_TIERS, IV4Quoter};
use crate::domain::error::AppError;
use crate::domain::models::{Quote, QuoteRequest, Venue};
use crate::repositories::quoter::Quoter;
use alloy::primitives::aliases::{I24, U24};
use alloy::primitives::{Address, U256};
use alloy::sol_types::SolValue;
use async_trait::async_trait;
use std::collections::HashMap;

pub struct UniV4Quoter {
    chains: HashMap<u64, ChainSetup>,
}

impl UniV4Quoter {
    pub fn new(chains: HashMap<u64, ChainSetup>) -> Self {
        Self { chains }
    }
}

#[async_trait]
impl Quoter for UniV4Quoter {
    fn venue(&self) -> Venue {
        Venue::UniV4
    }

    fn supports_chain(&self, chain_id: u64) -> bool {
        self.chains.contains_key(&chain_id)
    }

    async fn quote(&self, req: &QuoteRequest) -> Result<Quote, AppError> {
        let setup = self
            .chains
            .get(&req.chain_id)
            .ok_or(AppError::UnsupportedChain(req.chain_id))?;

        // V4 takes the input as a uint128, unlike UniV3's uint256, so an amount
        // that does not fit is rejected rather than silently truncated.
        let exact_amount: u128 = req
            .amount_in
            .try_into()
            .map_err(|_| AppError::BadRequest("amount_in exceeds uint128".into()))?;

        let best = best_tier(setup, req, exact_amount).await?;
        // Route layout: abi.encode(uint24 fee, int24 tickSpacing), 64 bytes.
        // `UniV4Adapter` derives currency ordering from the token addresses and
        // pins `hooks` to zero, so neither is encoded.
        let route = (U24::from(best.fee), I24::unchecked_from(best.tick_spacing))
            .abi_encode()
            .into();

        Ok(setup.build_quote(Venue::UniV4, req, best.amount_out, best.gas_estimate, route))
    }
}

/// A V4 `PoolKey` requires `currency0 < currency1`, and `zeroForOne` says which
/// side of that sorted pair the input is. `UniV4Adapter` derives both the same
/// way from the same two addresses, which is why neither is carried in the
/// route — the two cannot drift apart.
fn pool_currencies(token_in: Address, token_out: Address) -> (bool, Address, Address) {
    if token_in < token_out {
        (true, token_in, token_out)
    } else {
        (false, token_out, token_in)
    }
}

struct TierQuote {
    fee: u32,
    tick_spacing: i32,
    amount_out: U256,
    gas_estimate: u64,
}

/// Race all canonical (fee, tickSpacing) pairs and pick the highest-output
/// pool. Pairs with no initialized pool revert at the lens and are dropped.
/// Returns [`AppError::NoLiquidity`] if every pair fails.
///
/// `hooks` is pinned to the zero address: a hook pool can charge dynamic fees
/// or run arbitrary logic during the swap, and `UniV4Adapter` will not execute
/// against one, so quoting one would produce an unexecutable route.
async fn best_tier(
    setup: &ChainSetup,
    req: &QuoteRequest,
    exact_amount: u128,
) -> Result<TierQuote, AppError> {
    let quoter = IV4Quoter::new(setup.quoter_addr, &setup.provider);

    let (zero_for_one, currency0, currency1) = pool_currencies(req.token_in, req.token_out);

    let calls = FEE_TIERS.iter().map(|&(fee, tick_spacing)| {
        let params = IV4Quoter::QuoteExactSingleParams {
            poolKey: IV4Quoter::PoolKey {
                currency0,
                currency1,
                fee: U24::from(fee),
                tickSpacing: I24::unchecked_from(tick_spacing),
                hooks: Address::ZERO,
            },
            zeroForOne: zero_for_one,
            exactAmount: exact_amount,
            hookData: Default::default(),
        };
        let call = quoter.quoteExactInputSingle(params);
        async move {
            let r = call.call().await.ok()?;
            Some(TierQuote {
                fee,
                tick_spacing,
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
    use alloy::primitives::address;

    /// The route must decode as `(uint24, int24)` on the Solidity side, which
    /// is two 32-byte words. `UniV4Adapter.swap` reverts on any other shape.
    #[test]
    fn route_encodes_as_two_words() {
        let route = (U24::from(500u32), I24::unchecked_from(10i32)).abi_encode();
        assert_eq!(route.len(), 64);
        // int24 is sign-extended across its word; 10 is positive so the high
        // bytes stay zero and the value lands in the last byte.
        assert_eq!(route[63], 10);
        assert_eq!(route[31], 0xf4); // 500 = 0x01f4
        assert_eq!(route[30], 0x01);
    }

    /// Both directions must produce the same sorted key and flip only
    /// `zeroForOne`; a pool is identified by the unordered pair.
    #[test]
    fn pool_currencies_sorts_both_directions() {
        let lo = address!("0000000000000000000000000000000000000001");
        let hi = address!("0000000000000000000000000000000000000002");

        assert_eq!(pool_currencies(lo, hi), (true, lo, hi));
        assert_eq!(pool_currencies(hi, lo), (false, lo, hi));
    }
}
