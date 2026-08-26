use alloy::sol;

sol! {
    /// UniV3 QuoterV2. `quoteExactInputSingle` is non-view (state-mutating in
    /// the EVM trace) but is invoked by clients as a `staticcall`/`eth_call`.
    /// Returns amountOut, sqrtPriceX96After, initializedTicksCrossed and
    /// gasEstimate.
    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IQuoterV2 {
        struct QuoteExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint24 fee;
            uint160 sqrtPriceLimitX96;
        }

        function quoteExactInputSingle(QuoteExactInputSingleParams memory params)
            external
            returns (
                uint256 amountOut,
                uint160 sqrtPriceX96After,
                uint32 initializedTicksCrossed,
                uint256 gasEstimate
            );
    }
}

/// The four canonical UniV3 fee tiers, in 1e-6 units: 0.01%, 0.05%, 0.3%, 1%.
pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];
