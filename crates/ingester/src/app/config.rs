use crate::adapters::rpc::RpcConfig;
use crate::domain::error::IngesterError;
use crate::domain::models::parse_address;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct IngesterConfig {
    pub database_url: String,
    pub chains: Vec<ChainConfig>,
    /// Where `/metrics` is served; the process's only listener. Defaults to
    /// loopback. Under compose it is set to `0.0.0.0:<port>` and the host publish
    /// restricts it (see `shared::metrics::init`).
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    /// Override the shared bb8 pool's size. `None` keeps
    /// [`database::PoolCfg::indexer`].
    ///
    /// One pool serves every chain worker, so on a deployment with many chains a
    /// checkout can block for the pool's timeout and then fail into the retry
    /// policy, turning contention into minutes of stall. This is the escape
    /// hatch for that; note the real ceiling behind a transaction pooler is
    /// simultaneously executing queries, not connections, so raising it past the
    /// pooler's own size moves the queue rather than removing it.
    #[serde(default)]
    pub db_pool_size: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainConfig {
    pub chain_id: i64,
    pub rpc_url: String,
    pub pool_address: String,
    pub start_block: i64,
    #[serde(default = "default_reorg_depth")]
    pub reorg_depth: u64,
    /// Ceiling of the live tail's idle backoff, not a fixed period. A tick that
    /// committed rows or rewound a fork loops straight back; only an idle tick
    /// waits, starting at 50ms and doubling up to this.
    #[serde(default = "default_block_poll_ms")]
    pub block_poll_ms: u64,
    #[serde(default = "default_backfill_threshold")]
    pub backfill_threshold: u64,
    #[serde(default = "default_backfill_concurrency")]
    pub backfill_concurrency: usize,
    /// Block range per backfill chunk, and the cap on one live tick's span.
    ///
    /// Also the memory bound: the backfill decodes `backfill_concurrency` chunks
    /// at once, so peak resident rows are the product of the two. It no longer
    /// needs to be tuned below the provider's `eth_getLogs` cap — the adaptive
    /// window learns that once and remembers it — so this is commit granularity.
    #[serde(default = "default_chunk_blocks")]
    pub chunk_blocks: u64,
    /// Cap on simultaneous `eth_getBlockByNumber` calls when resolving block
    /// metadata. Unbounded fan-out would turn a single large chunk into thousands
    /// of concurrent requests and trigger rate limiting.
    #[serde(default = "default_meta_concurrency")]
    pub meta_concurrency: usize,
    /// Whole-request timeout for RPC calls. Without one, a half-open socket stalls
    /// the worker indefinitely while it holds the chain's advisory lock, so no
    /// standby can take over.
    #[serde(default = "default_rpc_timeout_ms")]
    pub rpc_timeout_ms: u64,
    #[serde(default = "default_rpc_connect_timeout_ms")]
    pub rpc_connect_timeout_ms: u64,
}

fn default_metrics_addr() -> String {
    "127.0.0.1:3013".into()
}
fn default_reorg_depth() -> u64 {
    32
}
fn default_block_poll_ms() -> u64 {
    2000
}
fn default_backfill_threshold() -> u64 {
    100
}
fn default_backfill_concurrency() -> usize {
    8
}
fn default_chunk_blocks() -> u64 {
    10_000
}
fn default_meta_concurrency() -> usize {
    16
}
fn default_rpc_timeout_ms() -> u64 {
    30_000
}
fn default_rpc_connect_timeout_ms() -> u64 {
    10_000
}

/// Strip credentials from an RPC URL before it reaches a log sink.
///
/// Alchemy, Infura and QuickNode all carry the API key in the URL path, so
/// logging `rpc_url` verbatim would publish the key to every log consumer.
pub fn redact_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(u) => match u.host_str() {
            Some(host) => format!("{}://{}", u.scheme(), host),
            None => u.scheme().to_string(),
        },
        // Not a URL, so nothing to leak; the validator rejects it.
        Err(_) => "<invalid>".to_string(),
    }
}

impl IngesterConfig {
    /// The pool to open: the shared indexer preset, resized if an operator asked.
    pub fn pool(&self) -> database::PoolCfg {
        let base = database::PoolCfg::indexer();
        match self.db_pool_size {
            Some(n) => base.with_max_size(n),
            None => base,
        }
    }

    /// Overlay env vars on top of TOML defaults, per chain. Convention:
    ///   INGESTER_CHAIN_<id>_POOL_ADDRESS=0x…
    ///   INGESTER_CHAIN_<id>_RPC_URL=http://…
    ///   INGESTER_CHAIN_<id>_START_BLOCK=12345
    ///
    /// A malformed `START_BLOCK` is an error rather than a fallback to the TOML
    /// value, so the process refuses to start instead of ingesting from the wrong
    /// height.
    pub fn apply_env_overlay(&mut self) -> Result<(), IngesterError> {
        // Chain-independent, so it does not go through `config_env::lookup`.
        if let Ok(v) = std::env::var("METRICS_ADDR") {
            self.metrics_addr = v;
        }
        if let Ok(v) = std::env::var("INGESTER_DB_POOL_SIZE") {
            self.db_pool_size = Some(v.parse::<u32>().map_err(|e| {
                IngesterError::Config(format!("INGESTER_DB_POOL_SIZE={:?}: {}", v, e))
            })?);
        }
        for c in &mut self.chains {
            if let Some(v) = shared::config_env::lookup("INGESTER", c.chain_id, "POOL_ADDRESS") {
                c.pool_address = v;
            }
            if let Some(v) = shared::config_env::lookup("INGESTER", c.chain_id, "RPC_URL") {
                c.rpc_url = v;
            }
            if let Some(v) = shared::config_env::lookup("INGESTER", c.chain_id, "START_BLOCK") {
                c.start_block = v.parse::<i64>().map_err(|e| {
                    IngesterError::Config(format!(
                        "INGESTER_CHAIN_{}_START_BLOCK={:?}: {}",
                        c.chain_id, v, e
                    ))
                })?;
            }
        }
        Ok(())
    }

    /// Reject configurations that would panic, hang or misbehave.
    ///
    /// Called before any worker spawns, so a typo fails the process at startup
    /// rather than after a standby has waited out the advisory lock.
    pub fn validate(&self) -> Result<(), IngesterError> {
        if self.database_url.trim().is_empty() {
            return Err(IngesterError::config("database_url is empty"));
        }
        if self.chains.is_empty() {
            return Err(IngesterError::config("no chains configured"));
        }
        // bb8 rejects a zero pool at build time, well after a standby has waited
        // out the advisory lock.
        if self.db_pool_size == Some(0) {
            return Err(IngesterError::config("db_pool_size must be > 0"));
        }
        let mut seen: HashSet<i64> = HashSet::new();
        for c in &self.chains {
            // Two workers on one chain do not race, since the advisory lock
            // serialises them, but the loser blocks indefinitely with no
            // diagnostic, which presents as a hang.
            if !seen.insert(c.chain_id) {
                return Err(IngesterError::config(format!(
                    "duplicate chain_id {}",
                    c.chain_id
                )));
            }
            c.validate()
                .map_err(|e| IngesterError::config(format!("chain {}: {}", c.chain_id, e)))?;
        }
        Ok(())
    }
}

impl From<&ChainConfig> for RpcConfig {
    fn from(c: &ChainConfig) -> Self {
        Self {
            url: c.rpc_url.clone(),
            request_timeout: Duration::from_millis(c.rpc_timeout_ms),
            connect_timeout: Duration::from_millis(c.rpc_connect_timeout_ms),
            meta_concurrency: c.meta_concurrency,
            chain_id: c.chain_id,
        }
    }
}

impl ChainConfig {
    /// Per-chain checks. Returns a bare reason; the caller prefixes the id.
    fn validate(&self) -> Result<(), String> {
        // Each of these is a divisor, a loop bound or a concurrency limit, and a
        // zero would panic or stall.
        let positive: [(u64, &str); 5] = [
            (self.chunk_blocks, "chunk_blocks"),
            (self.backfill_concurrency as u64, "backfill_concurrency"),
            (self.meta_concurrency as u64, "meta_concurrency"),
            (self.block_poll_ms, "block_poll_ms"),
            (self.rpc_timeout_ms, "rpc_timeout_ms"),
        ];
        for (value, name) in positive {
            if value == 0 {
                return Err(format!("{} must be > 0", name));
            }
        }
        if self.rpc_connect_timeout_ms == 0 {
            return Err("rpc_connect_timeout_ms must be > 0".into());
        }
        // `start_block - 1` is used as a lag baseline; a negative value wraps and
        // skips backfill.
        if self.start_block < 0 {
            return Err(format!(
                "start_block must be >= 0, got {}",
                self.start_block
            ));
        }
        // Backfill stops at `tip - reorg_depth`, so it can only reduce the lag to
        // `reorg_depth`. If that still exceeds the threshold the worker
        // oscillates between backfill and live.
        if self.backfill_threshold <= self.reorg_depth {
            return Err(format!(
                "backfill_threshold ({}) must exceed reorg_depth ({}), otherwise the \
                 worker oscillates between backfill and live",
                self.backfill_threshold, self.reorg_depth
            ));
        }
        parse_address(&self.pool_address).map_err(|e| e.to_string())?;
        url::Url::parse(&self.rpc_url).map_err(|e| format!("rpc_url: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(id: i64) -> ChainConfig {
        ChainConfig {
            chain_id: id,
            rpc_url: "https://rpc.example/v2/secret-key".into(),
            pool_address: "0x0000000000000000000000000000000000000abc".into(),
            start_block: 100,
            reorg_depth: default_reorg_depth(),
            block_poll_ms: default_block_poll_ms(),
            backfill_threshold: default_backfill_threshold(),
            backfill_concurrency: default_backfill_concurrency(),
            chunk_blocks: default_chunk_blocks(),
            meta_concurrency: default_meta_concurrency(),
            rpc_timeout_ms: default_rpc_timeout_ms(),
            rpc_connect_timeout_ms: default_rpc_connect_timeout_ms(),
        }
    }

    fn cfg(chains: Vec<ChainConfig>) -> IngesterConfig {
        IngesterConfig {
            database_url: "postgres://localhost/db".into(),
            chains,
            metrics_addr: default_metrics_addr(),
            db_pool_size: None,
        }
    }

    #[test]
    fn accepts_a_sane_config() {
        assert!(cfg(vec![chain(1)]).validate().is_ok());
    }

    /// `step_by(0)` panics inside the backfill chunker, which would end the
    /// process long after startup.
    #[test]
    fn rejects_zero_chunk_blocks() {
        let mut c = chain(1);
        c.chunk_blocks = 0;
        assert!(cfg(vec![c]).validate().is_err());
    }

    #[test]
    fn rejects_zero_concurrency() {
        let mut c = chain(1);
        c.backfill_concurrency = 0;
        assert!(cfg(vec![c]).validate().is_err());
    }

    /// The loser of the advisory lock waits indefinitely with no diagnostic, so a
    /// duplicate id presents as a hang rather than a misconfiguration.
    #[test]
    fn rejects_duplicate_chain_ids() {
        assert!(cfg(vec![chain(1), chain(1)]).validate().is_err());
    }

    /// `start_block - 1` is cast to u64 when computing lag, so a negative value
    /// wraps to `u64::MAX` and skips backfill.
    #[test]
    fn rejects_negative_start_block() {
        let mut c = chain(1);
        c.start_block = -1;
        assert!(cfg(vec![c]).validate().is_err());
    }

    /// Backfill can only close the gap to `reorg_depth`; if that still trips the
    /// threshold the worker oscillates between modes.
    #[test]
    fn rejects_threshold_below_reorg_depth() {
        let mut c = chain(1);
        c.reorg_depth = 100;
        c.backfill_threshold = 50;
        assert!(cfg(vec![c]).validate().is_err());
    }

    #[test]
    fn rejects_unparsable_pool_address() {
        let mut c = chain(1);
        c.pool_address = "not-an-address".into();
        assert!(cfg(vec![c]).validate().is_err());
    }

    /// Provider API keys live in the URL path, so logging the raw URL would
    /// publish them to every log consumer.
    #[test]
    fn redaction_drops_path_and_query() {
        assert_eq!(
            redact_url("https://eth-mainnet.g.alchemy.com/v2/SECRET"),
            "https://eth-mainnet.g.alchemy.com"
        );
        assert_eq!(
            redact_url("https://user:pw@node.example:8545/path?key=SECRET"),
            "https://node.example"
        );
    }

    /// Leaving the knob unset must not silently change the pool a deployment
    /// already runs on.
    #[test]
    fn an_unset_pool_size_keeps_the_shared_preset() {
        let got = cfg(vec![chain(1)]).pool();
        assert_eq!(got.max_size, database::PoolCfg::indexer().max_size);
        assert_eq!(got.min_idle, database::PoolCfg::indexer().min_idle);
    }

    #[test]
    fn an_explicit_pool_size_resizes_the_preset() {
        let mut c = cfg(vec![chain(1)]);
        c.db_pool_size = Some(32);
        assert_eq!(c.pool().max_size, 32);
    }

    /// bb8 rejects a zero pool when it builds, long after a standby has waited
    /// out the advisory lock.
    #[test]
    fn a_zero_pool_size_fails_validation() {
        let mut c = cfg(vec![chain(1)]);
        c.db_pool_size = Some(0);
        assert!(c.validate().is_err());
    }

    /// The adapter takes its own config so a TOML rename cannot break it; this
    /// is the one place the two are tied together.
    #[test]
    fn rpc_config_carries_the_timeouts_and_the_chain_id() {
        let c = chain(42161);
        let got = RpcConfig::from(&c);
        assert_eq!(got.chain_id, 42161);
        assert_eq!(got.url, c.rpc_url);
        assert_eq!(got.request_timeout.as_millis() as u64, c.rpc_timeout_ms);
        assert_eq!(
            got.connect_timeout.as_millis() as u64,
            c.rpc_connect_timeout_ms
        );
        assert_eq!(got.meta_concurrency, c.meta_concurrency);
    }
}
