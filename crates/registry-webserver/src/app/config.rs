//! Deployment configuration.
//!
//! TOML rather than the plain env vars the other webservers use, because this is
//! the one that carries a per-chain list: a registry describes every chain the
//! deployment serves, and an array does not fit an env var. The relayer loads its
//! config the same way and for the same reason.
//!
//! Deployed addresses still arrive from the environment — they change per
//! deployment while the file does not — through the same
//! `<PREFIX>_CHAIN_<id>_<FIELD>` convention every other binary uses. See
//! [`shared::config_env`].

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryConfig {
    pub database_url: String,
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    /// Where `/metrics` is served; see [`shared::metrics::default_addr`].
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    /// How long a catalog response is reused.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_s: u64,
    #[serde(default)]
    pub token_prices: TokenPricesCfg,
    pub chains: Vec<ChainCfg>,
}

/// One chain, as the deployment describes it.
///
/// Every field here is a property of the *chain and deployment*, identical for
/// every relayer serving it — which is why it lives in this service rather than
/// in a relayer an operator may be self-hosting.
#[derive(Debug, Clone, Deserialize)]
pub struct ChainCfg {
    pub chain_id: i64,
    /// Human-readable name, for a client to label the network.
    pub name: Option<String>,
    /// Browser-reachable RPC. Published to wallets; not what this service reads
    /// the chain with — see [`Self::apy_rpc_url`].
    ///
    /// A wallet installs this as the chain's endpoint via
    /// `wallet_addEthereumChain`, so it must stay general-purpose. The read-only
    /// proxy goes in [`Self::read_rpc_url`] instead.
    pub rpc_url: Option<String>,
    /// Read-only RPC the SDK uses for its own `eth_call`/`eth_getLogs` traffic,
    /// normally the `rpc-proxy` service.
    ///
    /// Separate from `rpc_url` for the same reason `apy_rpc_url` is: one field
    /// per audience. Pointing `rpc_url` here would install a rate-limited,
    /// read-only endpoint into every user's wallet as the chain's RPC — where it
    /// would be asked for `eth_sendRawTransaction`, `eth_subscribe` and
    /// background block polling, none of which it serves. Absent falls back to
    /// `rpc_url`, which is today's behaviour.
    pub read_rpc_url: Option<String>,
    pub explorer_url: Option<String>,
    pub permit2_address: Option<String>,
    /// The pool the deployment declares. A wallet cross-checks this against the
    /// `maspAddress` a relayer reports for itself; a mismatch means the relayer
    /// is pointed somewhere else and must not be trusted.
    pub masp_address: Option<String>,
    /// Depth of the commitment tree the deployment runs. Cross-checked the same
    /// way, since a wallet builds proofs against this shape.
    pub tree_depth: Option<u32>,
    /// `NativeAdapter`, enabling native-coin deposit and withdraw.
    pub native_adapter_address: Option<String>,
    /// `SwapWrapper`, enabling swaps.
    pub swap_wrapper_address: Option<String>,
    /// Endpoint the rate measurement reads, which needs archive state. Separate
    /// from `rpc_url`: that one is published to browsers, while this issues slow
    /// historical calls and is frequently a different, privileged node.
    ///
    /// Absent falls back to `rpc_url`, which answers on a chain whose public
    /// endpoint happens to serve state a window back and simply measures nothing
    /// where it does not. With neither set the chain is not measured at all,
    /// rather than the boot failing: the registry is what this service is for,
    /// and the rate is a badge on it.
    pub apy_rpc_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenPricesCfg {
    #[serde(default = "default_price_base_url")]
    pub base_url: String,
    #[serde(default = "default_price_ttl")]
    pub ttl_s: u64,
    #[serde(default = "default_price_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for TokenPricesCfg {
    fn default() -> Self {
        Self {
            base_url: default_price_base_url(),
            ttl_s: default_price_ttl(),
            timeout_ms: default_price_timeout_ms(),
        }
    }
}

fn default_bind_addr() -> String {
    "0.0.0.0:3005".into()
}
fn default_metrics_addr() -> String {
    shared::metrics::default_addr(3016)
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

impl RegistryConfig {
    /// Overlay the deployed, per-chain values from the environment.
    ///
    /// Only rewrites chains already declared in the TOML, matching every other
    /// binary: the file decides which chains exist, the environment supplies
    /// what a deployment stamped onto them.
    pub fn apply_env_overlay(&mut self) {
        if let Some(v) = shared::config_env::string("DATABASE_URL") {
            self.database_url = v;
        }
        if let Some(v) = shared::config_env::string("REGISTRY_BIND_ADDR") {
            self.bind_addr = v;
        }
        if let Some(v) = shared::config_env::string("METRICS_ADDR") {
            self.metrics_addr = v;
        }
        for c in &mut self.chains {
            let get = |field: &str| shared::config_env::lookup("REGISTRY", c.chain_id, field);
            if let Some(v) = get("RPC_URL") {
                c.rpc_url = Some(v);
            }
            if let Some(v) = get("READ_RPC_URL") {
                c.read_rpc_url = Some(v);
            }
            if let Some(v) = get("APY_RPC_URL") {
                c.apy_rpc_url = Some(v);
            }
            if let Some(v) = get("MASP_ADDRESS") {
                c.masp_address = Some(v);
            }
            if let Some(v) = get("PERMIT2_ADDRESS") {
                c.permit2_address = Some(v);
            }
            if let Some(v) = get("EXPLORER_URL") {
                c.explorer_url = Some(v);
            }
            if let Some(v) = get("NATIVE_ADAPTER_ADDRESS") {
                c.native_adapter_address = Some(v);
            }
            if let Some(v) = get("SWAP_WRAPPER_ADDRESS") {
                c.swap_wrapper_address = Some(v);
            }
        }
    }

    /// Reject a config that would serve nonsense, at boot rather than per
    /// request.
    pub fn validate(&self) -> Result<()> {
        if self.database_url.is_empty() {
            bail!("database_url is empty");
        }
        if self.chains.is_empty() {
            bail!("no chains configured: this service would publish an empty registry");
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.chains {
            if !seen.insert(c.chain_id) {
                bail!("chain {} is declared twice", c.chain_id);
            }
        }
        Ok(())
    }

    /// Load, overlay and validate, in that order — so an env-supplied value is
    /// checked too.
    pub fn load() -> Result<Self> {
        let mut cfg: Self = shared::config::load_toml("REGISTRY_CONFIG", "registry.toml")
            .context("load registry config")?;
        cfg.apply_env_overlay();
        cfg.validate().context("registry config")?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(chain_id: i64) -> ChainCfg {
        ChainCfg {
            chain_id,
            name: None,
            rpc_url: None,
            read_rpc_url: None,
            explorer_url: None,
            permit2_address: None,
            masp_address: None,
            tree_depth: None,
            native_adapter_address: None,
            swap_wrapper_address: None,
            apy_rpc_url: None,
        }
    }

    fn cfg(chains: Vec<ChainCfg>) -> RegistryConfig {
        RegistryConfig {
            database_url: "postgres://localhost/x".into(),
            bind_addr: default_bind_addr(),
            metrics_addr: default_metrics_addr(),
            cache_ttl_s: default_cache_ttl(),
            token_prices: TokenPricesCfg::default(),
            chains,
        }
    }

    #[test]
    fn test_a_described_deployment_validates() {
        assert!(cfg(vec![chain(31337), chain(31338)]).validate().is_ok());
    }

    /// Every route here answers from the chain list. With none, the service
    /// would boot and serve an empty registry, which reads to a wallet exactly
    /// like a deployment that supports nothing.
    #[test]
    fn test_no_chains_is_rejected() {
        let err = cfg(vec![]).validate().expect_err("must reject");
        assert!(format!("{err}").contains("no chains"), "{err}");
    }

    /// The overlay keys on chain id, so a duplicate would make
    /// `REGISTRY_CHAIN_<id>_*` silently apply to only one of the two.
    #[test]
    fn test_duplicate_chain_id_is_rejected() {
        let err = cfg(vec![chain(31337), chain(31337)])
            .validate()
            .expect_err("must reject");
        assert!(format!("{err}").contains("31337"), "{err}");
    }

    #[test]
    fn test_empty_database_url_is_rejected() {
        let mut c = cfg(vec![chain(31337)]);
        c.database_url = String::new();
        assert!(c.validate().is_err());
    }
}
