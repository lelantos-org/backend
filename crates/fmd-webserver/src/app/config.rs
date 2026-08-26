use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct FmdWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    /// Where `/metrics` is served. Defaults to loopback, which suits a bare
    /// process. Under compose this is set to `0.0.0.0:<port>` and the host publish
    /// restricts it to loopback; see `shared::metrics::init` for why the bind
    /// cannot enforce that itself.
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    #[serde(default = "default_lag_warn")]
    pub indexer_lag_warn_blocks: u64,
}

fn default_lag_warn() -> u64 {
    50
}

fn default_metrics_addr() -> String {
    "127.0.0.1:3011".into()
}

impl FmdWebserverConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3001".into()),
            metrics_addr: std::env::var("METRICS_ADDR").unwrap_or_else(|_| default_metrics_addr()),
            indexer_lag_warn_blocks: std::env::var("INDEXER_LAG_WARN_BLOCKS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_lag_warn),
        })
    }
}
