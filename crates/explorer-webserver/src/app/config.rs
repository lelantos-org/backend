//! Environment-derived configuration.
//!
//! Read through `shared::config_env` rather than `std::env::var`, so an empty
//! variable counts as unset and a malformed one fails the process instead of
//! quietly taking the default.

use anyhow::{Context, Result};
use serde::Deserialize;
use shared::config_env::{parse, string};

#[derive(Debug, Clone, Deserialize)]
pub struct ExplorerWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    /// Where `/metrics` is served; see [`shared::metrics::default_addr`].
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_s: u64,
    /// DefiLlama-compatible price API root.
    #[serde(default = "default_price_base_url")]
    pub price_base_url: String,
    /// How long a spot price is served without refetching. Longer than the
    /// response cache, since prices move far slower than chain data and every
    /// miss costs an upstream round-trip.
    #[serde(default = "default_price_ttl")]
    pub price_ttl_s: u64,
    /// Upstream deadline. Prices are decoration, so a slow provider must not
    /// hold an endpoint open.
    #[serde(default = "default_price_timeout_ms")]
    pub price_timeout_ms: u64,
}

fn default_cache_ttl() -> u64 {
    30
}

fn default_metrics_addr() -> String {
    shared::metrics::default_addr(3014)
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
            database_url: string("DATABASE_URL").context("DATABASE_URL")?,
            bind_addr: string("EXPLORER_BIND_ADDR").unwrap_or_else(|| "0.0.0.0:3002".into()),
            metrics_addr: string("METRICS_ADDR").unwrap_or_else(default_metrics_addr),
            cache_ttl_s: parse("CACHE_TTL_S")?.unwrap_or_else(default_cache_ttl),
            price_base_url: string("PRICE_BASE_URL").unwrap_or_else(default_price_base_url),
            price_ttl_s: parse("PRICE_TTL_S")?.unwrap_or_else(default_price_ttl),
            price_timeout_ms: parse("PRICE_TIMEOUT_MS")?.unwrap_or_else(default_price_timeout_ms),
        })
    }
}
