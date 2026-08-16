use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ExplorerWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_s: u64,
    /// DefiLlama-compatible price API root.
    #[serde(default = "default_price_base_url")]
    pub price_base_url: String,
    /// How long a spot price is served without refetching. Longer than the
    /// response cache: prices move far slower than chain data, and every miss
    /// costs an upstream round-trip.
    #[serde(default = "default_price_ttl")]
    pub price_ttl_s: u64,
    /// Upstream deadline. Prices are decoration — a slow provider must not
    /// hold an endpoint open.
    #[serde(default = "default_price_timeout_ms")]
    pub price_timeout_ms: u64,
}

fn default_cache_ttl() -> u64 {
    30
}

fn default_price_base_url() -> String {
    "https://coins.llama.fi".into()
}

fn default_price_ttl() -> u64 {
    300
}

fn default_price_timeout_ms() -> u64 {
    5_000
}

impl ExplorerWebserverConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL")?,
            bind_addr: std::env::var("EXPLORER_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:3002".into()),
            cache_ttl_s: std::env::var("CACHE_TTL_S")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_cache_ttl),
            price_base_url: std::env::var("PRICE_BASE_URL")
                .unwrap_or_else(|_| default_price_base_url()),
            price_ttl_s: std::env::var("PRICE_TTL_S")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_price_ttl),
            price_timeout_ms: std::env::var("PRICE_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(default_price_timeout_ms),
        })
    }
}
