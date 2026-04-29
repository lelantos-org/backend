use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ExplorerWebserverConfig {
    pub database_url: String,
    pub bind_addr: String,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_s: u64,
}

fn default_cache_ttl() -> u64 {
    30
}
