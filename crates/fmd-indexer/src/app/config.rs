use crate::domain::error::{FmdIndexerError, Result};
use serde::Deserialize;
use std::path::Path;

const DEFAULT_PATH: &str = "fmd-indexer.toml";
const ENV_PATH: &str = "FMD_INDEXER_CONFIG";
const ENV_DATABASE_URL: &str = "DATABASE_URL";

#[derive(Debug, Clone, Deserialize)]
pub struct FmdIndexerConfig {
    pub database_url: String,
    #[serde(default = "default_filter_workers")]
    pub filter_workers: usize,
    #[serde(default = "default_filter_batch")]
    pub filter_batch: usize,
    #[serde(default = "default_filter_tick_ms")]
    pub filter_tick_ms: u64,
    /// Consume-loop batch size. Defaults to `filter_batch`, but the two loops
    /// price it differently: for the filter it is a throughput knob, while for
    /// consume it bounds the widest tx that can be committed at all.
    #[serde(default)]
    pub consume_batch: Option<usize>,
    #[serde(default)]
    pub consume_tick_ms: Option<u64>,
}

impl FmdIndexerConfig {
    pub fn consume_batch(&self) -> usize {
        self.consume_batch.unwrap_or(self.filter_batch)
    }
    pub fn consume_tick_ms(&self) -> u64 {
        self.consume_tick_ms.unwrap_or(self.filter_tick_ms)
    }
}

impl FmdIndexerConfig {
    pub fn load() -> Result<Self> {
        let path = std::env::var(ENV_PATH).unwrap_or_else(|_| DEFAULT_PATH.to_string());
        if Path::new(&path).exists() {
            let txt = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&txt)?)
        } else {
            Ok(Self {
                database_url: std::env::var(ENV_DATABASE_URL)
                    .map_err(|_| FmdIndexerError::Config("DATABASE_URL not set".into()))?,
                filter_workers: default_filter_workers(),
                filter_batch: default_filter_batch(),
                filter_tick_ms: default_filter_tick_ms(),
                consume_batch: None,
                consume_tick_ms: None,
            })
        }
    }
}

fn default_filter_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
fn default_filter_batch() -> usize {
    1000
}
fn default_filter_tick_ms() -> u64 {
    500
}
