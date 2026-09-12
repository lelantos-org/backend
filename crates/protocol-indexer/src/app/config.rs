//! The TOML config, loaded through `shared::config`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ProtocolIndexerConfig {
    pub database_url: String,
    /// The chains with an RPC endpoint. A chain absent here still indexes its
    /// events, but loses *both* RPC-backed paths: its assets keep
    /// `decimals = NULL` and render no human-readable amount, and it is never
    /// polled for yield state, so `asset_yield.index_ray` stays NULL.
    #[serde(default)]
    pub chains: Vec<ChainCfg>,
    /// Idle *ceiling* between batches, not a fixed period; see `shared::tick`.
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
    /// Max `raw_events` rows consumed per chain per tick.
    #[serde(default = "default_batch")]
    pub batch: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainCfg {
    pub chain_id: i64,
    /// HTTP RPC, read by the metadata sweep (`decimals()`, `symbol()`, the
    /// vault's `name()`) and by the yield-state poller (`yieldState`).
    pub rpc_url: String,
}

impl ProtocolIndexerConfig {
    /// Same env-overlay convention as the other binaries:
    ///   PROTOCOL_INDEXER_CHAIN_<id>_RPC_URL=http://…
    ///
    /// Only rewrites chains already present in the TOML.
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            if let Some(v) = shared::config_env::lookup("PROTOCOL_INDEXER", c.chain_id, "RPC_URL") {
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
