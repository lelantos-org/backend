use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayerSubmitResponse {
    /// Tx hash returned once the on-chain `transact()` call confirms.
    pub tx_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    /// Crate version from `Cargo.toml`.
    pub version: &'static str,
    /// Short git commit SHA at build time, or `"unknown"` outside a repo.
    pub commit: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainsResponse {
    pub chains: Vec<ChainHealth>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainHealth {
    pub chain_id: i64,
    pub committed_count: i64,
    pub current_root_hex: String,
    /// EIP-55 checksummed MASP pool address.
    pub masp_address: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeQuote {
    pub token_symbol: String,
    pub token_address: String,
    pub decimals: u8,
    /// Base-unit U256 as decimal string.
    pub amount: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateResponse {
    pub gas_used: u64,
    pub effective_gas_price_wei: String,
    pub total_native_wei: String,
    /// Per-chain markup applied (bps; 1000 = 10%).
    pub markup_bps: u32,
    /// Unix seconds (server time) when this quote was produced.
    pub quoted_at: u64,
    pub fees: Vec<FeeQuote>,
}
