use serde::Deserialize;

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
