use alloy::sol;

sol! {
    /// UniV4 `V4Quoter` lens. Like `IQuoterV2` it is non-view (it unlocks the
    /// PoolManager and reverts internally to return the result) but is invoked
    /// by clients as an `eth_call`.
    ///
    /// `Currency` is a value type over `address`, so it is ABI-identical to
    /// `address` here. The struct layout below matches the routers and quoters
    /// deployed on mainnet, Arbitrum and Base; newer upstream revisions add
    /// per-hop slippage fields that those deployments do not carry.
    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IV4Quoter {
        struct PoolKey {
            address currency0;
            address currency1;
            uint24 fee;
            int24 tickSpacing;
            address hooks;
        }

        struct QuoteExactSingleParams {
            PoolKey poolKey;
            bool zeroForOne;
            uint128 exactAmount;
            bytes hookData;
        }

        function quoteExactInputSingle(QuoteExactSingleParams memory params)
            external
            returns (uint256 amountOut, uint256 gasEstimate);
    }
}

/// The four canonical fee tiers paired with the tick spacing a vanilla V4 pool
/// uses for each, mirroring the UniV3 tiers. A V4 pool is keyed by both, so the
/// fan-out enumerates pairs rather than fees alone.
///
/// Pools created with a non-standard spacing are not reachable this way, the
/// same way `UniV3Quoter` only reaches the canonical tiers.
pub const FEE_TIERS: [(u32, i32); 4] = [(100, 1), (500, 10), (3000, 60), (10000, 200)];
