use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct RelayerConfig {
    pub database_url: String,
    pub listen_addr: String,
    /// Per-chain settings; relayer holds one in-memory tree mirror + one
    /// alloy provider per entry.
    pub chains: Vec<ChainCfg>,
    pub prover: ProverCfg,
    #[serde(default)]
    pub price_oracle: PriceOracleCfg,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainCfg {
    pub chain_id: i64,
    pub rpc_url: String,
    /// MASP pool address (target of `transact` calls).
    pub pool_address: String,
    /// Relayer signer key (32-byte hex). MUST match the on-chain bound
    /// `relayer` address that wallets pin in their transact_2x2 proofs.
    pub signer_key_hex: String,
    /// Receipt poll budget in seconds. Submission revert ⇒ in-memory tree
    /// rolls back two leaves and the HTTP caller gets 502.
    #[serde(default = "default_receipt_timeout_s")]
    pub receipt_timeout_s: u64,
    /// Receipt poll interval (ms). Drives alloy's pending-tx watcher cadence.
    /// Pick ~1/4 of block time so confirmation latency tracks block production
    /// without hammering the RPC.
    #[serde(default = "default_receipt_poll_interval_ms")]
    pub receipt_poll_interval_ms: u64,
    /// Cron interval (seconds) for the shield flush worker. The worker
    /// polls `intent_escrowed_events` for unflushed intents, batches up
    /// to `flush_max_n`, and submits one `flushBatch` tx.
    #[serde(default = "default_flush_interval_s")]
    pub flush_interval_s: u64,
    /// Upper bound on per-flush batch size. Capped at the contract's
    /// `MAX_N_BATCH = 8`.
    #[serde(default = "default_flush_max_n")]
    pub flush_max_n: usize,
    /// Optional. When set, enables `/v1/swap` for this chain. Submitter
    /// targets this address for swap calldata; spend ops still target
    /// `pool_address`. Mirrors `SwapWrapper.sol` deployed alongside MASP.
    #[serde(default)]
    pub swap_wrapper_address: Option<String>,
    /// Oracle base symbol for the chain's native gas token (e.g. "ETH",
    /// "BNB", "MATIC"). Used as `base` in Coinbase price lookups.
    #[serde(default = "default_native_symbol")]
    pub native_symbol: String,
    /// Native token decimals (18 for all EVM chains today).
    #[serde(default = "default_native_decimals")]
    pub native_decimals: u8,
    /// Per-chain markup applied on top of the raw gas cost. bps: 1000 = 10%.
    #[serde(default = "default_fee_markup_bps")]
    pub fee_markup_bps: u32,
    /// Accepted fee tokens for `/v1/spend/estimate` and `/v1/swap/estimate`.
    #[serde(default)]
    pub accepted_fee_tokens: Vec<FeeTokenCfg>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FeeTokenCfg {
    pub symbol: String,
    pub address: String,
    pub decimals: u8,
    /// Oracle quote symbol, e.g. "USDC".
    pub quote_symbol: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PriceOracleCfg {
    #[serde(default = "default_oracle_base_url")]
    pub base_url: String,
    #[serde(default = "default_oracle_endpoint")]
    pub endpoint: String, // "spot" | "buy"
    #[serde(default = "default_oracle_ttl_s")]
    pub cache_ttl_s: u64,
    #[serde(default = "default_oracle_max_stale_s")]
    pub max_stale_s: u64,
    #[serde(default = "default_oracle_allow_usd_cross")]
    pub allow_usd_cross: bool,
}

impl Default for PriceOracleCfg {
    fn default() -> Self {
        Self {
            base_url: default_oracle_base_url(),
            endpoint: default_oracle_endpoint(),
            cache_ttl_s: default_oracle_ttl_s(),
            max_stale_s: default_oracle_max_stale_s(),
            allow_usd_cross: default_oracle_allow_usd_cross(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProverCfg {
    /// `circuits/build/tree_update_js/tree_update.wasm`.
    pub wasm_path: PathBuf,
    /// `circuits/build/tree_update.r1cs`.
    pub r1cs_path: PathBuf,
    /// `circuits/build/tree_update_final.zkey` (snarkjs-compatible).
    pub zkey_path: PathBuf,
}

fn default_receipt_timeout_s() -> u64 {
    60
}

fn default_receipt_poll_interval_ms() -> u64 {
    250
}

fn default_flush_interval_s() -> u64 {
    30
}

fn default_flush_max_n() -> usize {
    8
}

fn default_native_symbol() -> String {
    "ETH".to_string()
}

fn default_native_decimals() -> u8 {
    18
}

fn default_fee_markup_bps() -> u32 {
    1000
}

fn default_oracle_base_url() -> String {
    "https://api.coinbase.com/v2".to_string()
}

fn default_oracle_endpoint() -> String {
    "spot".to_string()
}

fn default_oracle_ttl_s() -> u64 {
    300
}

fn default_oracle_max_stale_s() -> u64 {
    300
}

fn default_oracle_allow_usd_cross() -> bool {
    true
}

impl RelayerConfig {
    /// Overlay env vars on top of TOML defaults, per chain. Convention:
    ///   RELAYER_CHAIN_<id>_POOL_ADDRESS=0x…
    ///   RELAYER_CHAIN_<id>_RPC_URL=http://…
    ///   RELAYER_CHAIN_<id>_SIGNER_KEY=0x…
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "POOL_ADDRESS") {
                c.pool_address = v;
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "RPC_URL") {
                c.rpc_url = v;
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "SIGNER_KEY") {
                c.signer_key_hex = v;
            }
            if let Some(v) =
                shared::config_env::lookup("RELAYER", c.chain_id, "SWAP_WRAPPER_ADDRESS")
            {
                c.swap_wrapper_address = Some(v);
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "NATIVE_SYMBOL") {
                c.native_symbol = v;
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "FEE_MARKUP_BPS")
                && let Ok(n) = v.parse::<u32>()
            {
                c.fee_markup_bps = n;
            }
        }
    }
}
