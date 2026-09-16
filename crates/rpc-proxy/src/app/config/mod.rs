//! Configuration and its environment overlay.
//!
//! TOML supplies static defaults; per-chain environment variables override
//! them. A variable that is set but does not parse fails startup rather than
//! falling back to the TOML value: the fields it guards are the upstream URL,
//! which carries the API key, and the contract allowlist. Silently keeping the
//! previous value would serve traffic that differs from the configuration the
//! operator believes is in effect.

use crate::adapters::ratelimit::HeaderPosition;
use crate::domain::error::{AppError, AppResult};
use alloy::primitives::Address;
use serde::Deserialize;
use shared::config_env::{self, ParseError};
use std::collections::HashSet;
use std::str::FromStr;

const PREFIX: &str = "RPC_PROXY";

#[derive(Debug, Deserialize, Clone)]
pub struct RpcProxyConfig {
    pub listen_addr: String,
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    /// Header carrying the real client address, when a proxy sits in front.
    ///
    /// Unset means "use the socket peer", which is right for a bare process.
    /// Configured rather than sniffed: honouring a forwarding header when
    /// nothing sets it would let any caller choose its own rate-limit bucket.
    ///
    /// Behind Cloudflare this is `CF-Connecting-IP`, which Cloudflare
    /// *overwrites* on every request and so cannot be spoofed through it. That
    /// guarantee holds only while the origin is unreachable except through
    /// Cloudflare — the compose deployment publishes on loopback behind Caddy,
    /// which is what keeps it true.
    #[serde(default)]
    pub trusted_client_ip_header: Option<String>,
    /// Which entry of the header to believe when it carries a list.
    ///
    /// Irrelevant for a single-valued header like `CF-Connecting-IP`. It
    /// matters for `X-Forwarded-For`, which every hop appends to: see
    /// [`HeaderPosition`].
    #[serde(default)]
    pub trusted_client_ip_position: HeaderPosition,
    /// Largest client batch accepted.
    ///
    /// Set well above the SDK's `batchSize` of 20. The two are deployed
    /// independently, and this margin is what keeps a deployed SDK working when
    /// this value changes.
    #[serde(default = "default_max_batch")]
    pub max_batch: usize,
    #[serde(default = "default_max_log_range")]
    pub max_log_range: u64,
    #[serde(default = "default_max_call_data_bytes")]
    pub max_call_data_bytes: usize,
    #[serde(default = "default_max_call_gas")]
    pub max_call_gas: u64,
    /// Ceiling on upstream calls in flight per chain.
    ///
    /// Backpressure, not a rate limit: the rate limiters bound spend over time,
    /// this bounds how much of it can be outstanding at once. See
    /// [`crate::adapters::upstream::HttpUpstream`].
    #[serde(default = "default_upstream_max_inflight")]
    pub upstream_max_inflight: usize,
    #[serde(default)]
    pub rate_limit: RateLimitCfg,
    /// Must be non-empty.
    pub chains: Vec<ChainCfg>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitCfg {
    #[serde(default = "default_client_ups")]
    pub client_units_per_second: u32,
    #[serde(default = "default_client_burst")]
    pub client_burst_units: u32,
    #[serde(default = "default_client_long_units")]
    pub client_long_units: u32,
    #[serde(default = "default_client_long_window_s")]
    pub client_long_window_s: u64,
}

impl Default for RateLimitCfg {
    fn default() -> Self {
        Self {
            client_units_per_second: default_client_ups(),
            client_burst_units: default_client_burst(),
            client_long_units: default_client_long_units(),
            client_long_window_s: default_client_long_window_s(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainCfg {
    pub chain_id: u64,
    /// The paid endpoint. **A secret**: the API key lives in the URL, so this is
    /// mounted from a file rather than passed as an environment variable in
    /// production, so it does not appear in `docker inspect` or the process
    /// environment.
    pub upstream_url: String,
    /// Whether the primary retains historical state. Required for the yield
    /// index's reads at explicit heights; see [`ChainCfg::has_archive`].
    #[serde(default = "default_true")]
    pub upstream_archive: bool,
    /// A public endpoint to fall back to when the primary is unreachable.
    #[serde(default)]
    pub fallback_url: Option<String>,
    /// Public endpoints normally prune, so the default is `false`. A historical
    /// read is never sent to a non-archive endpoint: it would come back as a
    /// JSON-RPC error, which this service treats as a verdict and would return
    /// as though the chain had said it.
    #[serde(default)]
    pub fallback_archive: bool,
    /// How deep a block must be before a reorg cannot reach it.
    ///
    /// The default is mainnet-flavoured. L2s produce blocks far faster, so the
    /// same number of blocks is seconds rather than minutes there and wants a
    /// larger value; see this crate's README.
    #[serde(default = "default_reorg_depth")]
    pub reorg_depth: u64,
    /// Block the deployment's contracts were created in.
    ///
    /// A historical state read below this is asking a paid archive node about
    /// state that cannot exist — every `eth_call` target here is one of our
    /// contracts. Unset disables the floor, which is what a deployment that has
    /// not recorded its deploy height gets.
    #[serde(default)]
    pub deploy_block: Option<u64>,
    #[serde(default = "default_upstream_ups")]
    pub upstream_units_per_second: u32,
    #[serde(default = "default_upstream_burst")]
    pub upstream_burst_units: u32,
    pub masp_address: Address,
    pub permit2_address: Address,
    /// The ERC-20s the SDK reads. Generated at template time; see the README.
    #[serde(default)]
    pub erc20_seed: Vec<Address>,
    /// `ERC4626Venue` addresses. These appear in no other config file — they are
    /// `CREATE`-derived at deploy and only logged — so they are generated from
    /// the registry's `/v1/assets` at template time.
    #[serde(default)]
    pub venue_seed: Vec<Address>,
}

impl ChainCfg {
    /// Whether any configured endpoint can answer a historical read.
    pub fn has_archive(&self) -> bool {
        self.upstream_archive || (self.fallback_url.is_some() && self.fallback_archive)
    }
}

fn default_metrics_addr() -> String {
    shared::metrics::default_addr(3017)
}

fn default_max_batch() -> usize {
    100
}

fn default_max_log_range() -> u64 {
    5_000
}

fn default_max_call_data_bytes() -> usize {
    8 * 1024
}

fn default_max_call_gas() -> u64 {
    50_000_000
}

fn default_upstream_max_inflight() -> usize {
    64
}

fn default_reorg_depth() -> u64 {
    64
}

fn default_client_ups() -> u32 {
    30
}

fn default_client_burst() -> u32 {
    180
}

fn default_client_long_units() -> u32 {
    3_000
}

fn default_client_long_window_s() -> u64 {
    300
}

fn default_upstream_ups() -> u32 {
    300
}

fn default_upstream_burst() -> u32 {
    1_500
}

fn default_true() -> bool {
    true
}

impl RpcProxyConfig {
    /// Overlay `RPC_PROXY_CHAIN_<id>_<FIELD>` on the TOML, per chain.
    ///
    /// Only rewrites chains already declared in the TOML; a variable naming an
    /// undeclared chain is discarded.
    pub fn apply_env_overlay(&mut self) -> Result<(), ParseError> {
        for c in &mut self.chains {
            let id = c.chain_id as i64;
            if let Some(url) = config_env::lookup(PREFIX, id, "UPSTREAM_URL") {
                c.upstream_url = url;
            }
            if let Some(url) = config_env::lookup(PREFIX, id, "FALLBACK_URL") {
                c.fallback_url = Some(url);
            }
            overlay(id, "UPSTREAM_ARCHIVE", &mut c.upstream_archive)?;
            overlay(id, "FALLBACK_ARCHIVE", &mut c.fallback_archive)?;
            overlay(id, "REORG_DEPTH", &mut c.reorg_depth)?;
            if let Some(v) = config_env::lookup_parse::<u64>(PREFIX, id, "DEPLOY_BLOCK")? {
                c.deploy_block = Some(v);
            }
            overlay(
                id,
                "UPSTREAM_UNITS_PER_SECOND",
                &mut c.upstream_units_per_second,
            )?;
            overlay(id, "UPSTREAM_BURST_UNITS", &mut c.upstream_burst_units)?;
            overlay(id, "MASP_ADDRESS", &mut c.masp_address)?;
            overlay(id, "PERMIT2_ADDRESS", &mut c.permit2_address)?;
            overlay_list(id, "ERC20_SEED", &mut c.erc20_seed)?;
            overlay_list(id, "VENUE_SEED", &mut c.venue_seed)?;
        }
        Ok(())
    }

    /// Reject a config that would start but not behave as written.
    ///
    /// Run after the overlay, since the overlay is what usually introduces the
    /// mistake.
    pub fn validate(&self) -> AppResult<()> {
        if self.chains.is_empty() {
            return Err(AppError::Internal("no chains configured".into()));
        }

        // Every one of these is a ceiling on what a single unauthenticated
        // request may cost, and every one is overridable from the environment.
        // Bounded here because the failure mode is silent: a value raised by a
        // stray variable starts cleanly and shows up as a provider bill.
        bound("max_batch", self.max_batch, 1, 1_000)?;
        bound("max_log_range", self.max_log_range, 1, 100_000)?;
        bound(
            "max_call_data_bytes",
            self.max_call_data_bytes,
            1,
            128 * 1024,
        )?;
        bound("max_call_gas", self.max_call_gas, 1, 100_000_000)?;
        bound(
            "upstream_max_inflight",
            self.upstream_max_inflight,
            1,
            1_024,
        )?;

        let mut seen = HashSet::new();
        for c in &self.chains {
            // Two blocks for one id would be last-wins through the chain map,
            // so a stale block left above a corrected one would silently decide
            // which upstream every request for that chain went to.
            if !seen.insert(c.chain_id) {
                return Err(AppError::Internal(format!(
                    "chain {} is declared more than once",
                    c.chain_id
                )));
            }
            if c.upstream_url.trim().is_empty() {
                return Err(AppError::Internal(format!(
                    "chain {}: upstream_url is empty",
                    c.chain_id
                )));
            }
            // An address listed in both seeds is a generator bug, and which
            // class it lands in decides which functions it exposes.
            let mut addrs = HashSet::new();
            for a in c.erc20_seed.iter().chain(&c.venue_seed) {
                if !addrs.insert(*a) {
                    return Err(AppError::Internal(format!(
                        "chain {}: address {a} appears twice in the seeds",
                        c.chain_id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Reject a cost ceiling outside its supported range.
///
/// The upper bound is the point of this: a zero is obviously broken and fails
/// fast, but a `max_log_range` of `u64::MAX` starts cleanly and quietly removes
/// the check it exists to be.
fn bound<T>(field: &str, got: T, min: T, max: T) -> AppResult<()>
where
    T: PartialOrd + std::fmt::Display,
{
    if got < min || got > max {
        return Err(AppError::Internal(format!(
            "{field} must be between {min} and {max}, got {got}"
        )));
    }
    Ok(())
}

/// Overlay one required field. Unset leaves the TOML value; set-but-malformed is
/// an error, never a silent fallback.
fn overlay<T: FromStr>(chain_id: i64, field: &str, slot: &mut T) -> Result<(), ParseError> {
    if let Some(v) = config_env::lookup_parse::<T>(PREFIX, chain_id, field)? {
        *slot = v;
    }
    Ok(())
}

/// Overlay a comma-separated list, replacing the TOML value wholesale.
///
/// Replacing rather than appending: a seed is a complete statement of what this
/// chain serves, and a union would make it impossible to remove an address
/// without editing two places.
fn overlay_list<T: FromStr>(
    chain_id: i64,
    field: &str,
    slot: &mut Vec<T>,
) -> Result<(), ParseError> {
    let Some(raw) = config_env::lookup(PREFIX, chain_id, field) else {
        return Ok(());
    };
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        out.push(part.parse::<T>().map_err(|_| ParseError {
            key: format!("{PREFIX}_CHAIN_{chain_id}_{field}"),
            raw: part.to_string(),
            expected: std::any::type_name::<T>(),
        })?);
    }
    *slot = out;
    Ok(())
}

#[cfg(test)]
mod tests;
