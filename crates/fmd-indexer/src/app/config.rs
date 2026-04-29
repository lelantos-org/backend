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
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
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
                retention_days: default_retention_days(),
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
fn default_retention_days() -> u32 {
    30
}
