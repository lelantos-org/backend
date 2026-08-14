use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RiskWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    /// Verdict cache TTL. With no write API there is nothing to invalidate,
    /// so this is exactly the worst-case lag between a row appearing in
    /// `screened_addresses` and the service acting on it — per replica.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_s: u64,
}

fn default_cache_ttl() -> u64 {
    60
}

impl RiskWebserverConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL")?,
            bind_addr: std::env::var("RISK_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3004".into()),
            cache_ttl_s: std::env::var("CACHE_TTL_S")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_cache_ttl),
        })
    }
}
