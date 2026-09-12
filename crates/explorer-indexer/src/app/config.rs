use serde::Deserialize;

/// No `chains` block, and no RPC.
///
/// This binary aggregates rows the ingester has already written; everything that
/// reads a chain — ERC20 `decimals()` and the yield-index poll — moved to
/// protocol-indexer. A chain needs no entry here to be indexed.
#[derive(Debug, Clone, Deserialize)]
pub struct ExplorerIndexerConfig {
    pub database_url: String,
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
    #[serde(default = "default_batch")]
    pub batch: i64,
}

fn default_tick_ms() -> u64 {
    1000
}

fn default_batch() -> i64 {
    500
}
