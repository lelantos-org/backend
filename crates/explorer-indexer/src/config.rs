use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ExplorerIndexerConfig {
    pub database_url: String,
    /// Per-chain RPC, used only to read ERC20 `decimals()` for registered
    /// assets. A chain absent here still indexes; its assets keep
    /// `decimals = NULL` and render no human amount.
    #[serde(default)]
    pub chains: Vec<ChainCfg>,
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
    #[serde(default = "default_batch")]
    pub batch: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainCfg {
    pub chain_id: i64,
    pub rpc_url: String,
}

impl ExplorerIndexerConfig {
    /// Same env-overlay convention as the other binaries:
    ///   EXPLORER_INDEXER_CHAIN_<id>_RPC_URL=http://…
    /// Only rewrites chains already present in the TOML.
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            if let Some(v) = shared::config_env::lookup("EXPLORER_INDEXER", c.chain_id, "RPC_URL") {
                c.rpc_url = v;
            }
        }
    }
}

fn default_tick_ms() -> u64 {
    1000
}

fn default_batch() -> i64 {
    500
}
