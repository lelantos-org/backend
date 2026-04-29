use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct IngesterConfig {
    pub database_url: String,
    pub chains: Vec<ChainConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainConfig {
    pub chain_id: i64,
    pub rpc_url: String,
    pub pool_address: String,
    pub start_block: i64,
    #[serde(default = "default_reorg_depth")]
    pub reorg_depth: u64,
    #[serde(default = "default_block_poll_ms")]
    pub block_poll_ms: u64,
    #[serde(default = "default_backfill_threshold")]
    pub backfill_threshold: u64,
    #[serde(default = "default_backfill_concurrency")]
    pub backfill_concurrency: usize,
    #[serde(default = "default_chunk_blocks")]
    pub chunk_blocks: u64,
}

fn default_reorg_depth() -> u64 {
    32
}
fn default_block_poll_ms() -> u64 {
    2000
}
fn default_backfill_threshold() -> u64 {
    100
}
fn default_backfill_concurrency() -> usize {
    8
}
fn default_chunk_blocks() -> u64 {
    50_000
}

impl IngesterConfig {
    /// Overlay env vars on top of TOML defaults, per chain. Convention:
    ///   INGESTER_CHAIN_<id>_POOL_ADDRESS=0x…
    ///   INGESTER_CHAIN_<id>_RPC_URL=http://…
    ///   INGESTER_CHAIN_<id>_START_BLOCK=12345
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            if let Some(v) = shared::config_env::lookup("INGESTER", c.chain_id, "POOL_ADDRESS") {
                c.pool_address = v;
            }
            if let Some(v) = shared::config_env::lookup("INGESTER", c.chain_id, "RPC_URL") {
                c.rpc_url = v;
            }
            if let Some(v) = shared::config_env::lookup("INGESTER", c.chain_id, "START_BLOCK")
                && let Ok(n) = v.parse::<i64>()
            {
                c.start_block = n;
            }
        }
    }
}
