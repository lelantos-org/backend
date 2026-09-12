//! Process configuration, read from the environment through
//! [`shared::config_env`] so an empty variable counts as unset and a malformed
//! one fails startup rather than silently defaulting.

use anyhow::{Context, Result};
use serde::Deserialize;
use shared::config_env::{parse, string};

#[derive(Debug, Clone, Deserialize)]
pub struct FmdWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    /// Where `/metrics` is served; see [`shared::metrics::default_addr`].
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    #[serde(default = "default_lag_warn")]
    pub indexer_lag_warn_blocks: u64,
}

fn default_lag_warn() -> u64 {
    50
}

fn default_metrics_addr() -> String {
    shared::metrics::default_addr(3011)
}

impl FmdWebserverConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: string("DATABASE_URL").context("DATABASE_URL")?,
            bind_addr: string("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:3001".into()),
            metrics_addr: string("METRICS_ADDR").unwrap_or_else(default_metrics_addr),
            indexer_lag_warn_blocks: parse("INDEXER_LAG_WARN_BLOCKS")?
                .unwrap_or_else(default_lag_warn),
        })
    }
}
