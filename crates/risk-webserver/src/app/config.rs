use anyhow::{Context, Result};
use serde::Deserialize;
use shared::config_env::{parse, string};

#[derive(Debug, Clone, Deserialize)]
pub struct RiskWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    /// Where `/metrics` is served; see [`shared::metrics::default_addr`].
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    /// Verdict cache TTL. With no write API there is nothing to invalidate, so
    /// this is the worst-case per-replica lag between a row appearing in
    /// `screened_addresses` and the service acting on it.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_s: u64,
}

fn default_cache_ttl() -> u64 {
    60
}

fn default_metrics_addr() -> String {
    shared::metrics::default_addr(3015)
}

impl RiskWebserverConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: string("DATABASE_URL").context("DATABASE_URL")?,
            bind_addr: string("RISK_BIND_ADDR").unwrap_or_else(|| "0.0.0.0:3004".into()),
            metrics_addr: string("METRICS_ADDR").unwrap_or_else(default_metrics_addr),
            cache_ttl_s: parse("CACHE_TTL_S")?.unwrap_or_else(default_cache_ttl),
        })
    }
}
