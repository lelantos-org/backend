use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct FmdWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    #[serde(default = "default_lag_warn")]
    pub indexer_lag_warn_blocks: u64,
}

fn default_lag_warn() -> u64 {
    50
}

impl FmdWebserverConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3001".into()),
            indexer_lag_warn_blocks: std::env::var("INDEXER_LAG_WARN_BLOCKS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_lag_warn),
        })
    }
}
