//! The TOML config, loaded through `shared::config`.

use alloy::primitives::Address;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct ProtocolIndexerConfig {
    pub database_url: String,
    /// The chains with an RPC endpoint. A chain absent here still indexes its
    /// events, but loses *both* RPC-backed paths: its assets keep
    /// `decimals = NULL` and render no human-readable amount, and it is never
    /// polled for yield state, so `asset_yield.index_ray` stays NULL.
    #[serde(default)]
    pub chains: Vec<ChainCfg>,
    /// Idle *ceiling* between batches, not a fixed period; see `shared::tick`.
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
    /// Max `raw_events` rows consumed per chain per tick.
    #[serde(default = "default_batch")]
    pub batch: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainCfg {
    pub chain_id: i64,
    /// HTTP RPC, read by the metadata sweep (`decimals()`, `symbol()`, the
    /// vault's `name()`) and by the yield-state poller (`yieldState`).
    pub rpc_url: String,
    /// The `LelantosGovernor` whose proposal and vote logs are indexed.
    ///
    /// Governance events are accepted only from this emitter: their signatures
    /// are OpenZeppelin's and generic enough for any contract to emit, and a
    /// redeployed governor keeps the old one's logs in `raw_events` under
    /// proposal ids a new proposal can collide with. Absent or zero indexes no
    /// governance for the chain.
    #[serde(default)]
    pub governor_address: Option<String>,
}

impl ProtocolIndexerConfig {
    /// Same env-overlay convention as the other binaries:
    ///
    /// ```text
    /// PROTOCOL_INDEXER_CHAIN_<id>_RPC_URL=http://…
    /// PROTOCOL_INDEXER_CHAIN_<id>_GOVERNOR_ADDRESS=0x…
    /// ```
    ///
    /// Only rewrites chains already present in the TOML.
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            if let Some(v) = shared::config_env::lookup("PROTOCOL_INDEXER", c.chain_id, "RPC_URL") {
                c.rpc_url = v;
            }
            if let Some(v) =
                shared::config_env::lookup("PROTOCOL_INDEXER", c.chain_id, "GOVERNOR_ADDRESS")
            {
                c.governor_address = Some(v);
            }
        }
    }

    /// The governor per chain, zero and absent entries left out.
    ///
    /// Fails on a malformed address rather than skipping it: a typo would
    /// otherwise switch governance indexing off without a word.
    pub fn governors(&self) -> Result<HashMap<i64, Address>, String> {
        let mut out = HashMap::new();
        for c in &self.chains {
            let Some(raw) = c.governor_address.as_deref() else {
                continue;
            };
            let addr: Address = raw
                .parse()
                .map_err(|e| format!("chain {}: governor_address {raw}: {e}", c.chain_id))?;
            if !addr.is_zero() {
                out.insert(c.chain_id, addr);
            }
        }
        Ok(out)
    }
}

fn default_tick_ms() -> u64 {
    1000
}

fn default_batch() -> i64 {
    500
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(governor: Option<&str>) -> ProtocolIndexerConfig {
        ProtocolIndexerConfig {
            database_url: "postgres://x".into(),
            chains: vec![ChainCfg {
                chain_id: 31337,
                rpc_url: "http://anvil:8545".into(),
                governor_address: governor.map(str::to_string),
            }],
            tick_ms: default_tick_ms(),
            batch: default_batch(),
        }
    }

    #[test]
    fn a_configured_governor_is_keyed_by_chain() {
        let got = cfg(Some("0x0000000000000000000000000000000000000abc"))
            .governors()
            .unwrap();
        assert_eq!(
            got.get(&31337).copied(),
            Some(
                "0x0000000000000000000000000000000000000abc"
                    .parse()
                    .unwrap()
            )
        );
    }

    /// The dev TOML declares zero so the overlay has a key to rewrite.
    #[test]
    fn zero_and_absent_governors_are_left_out() {
        assert!(cfg(None).governors().unwrap().is_empty());
        assert!(
            cfg(Some("0x0000000000000000000000000000000000000000"))
                .governors()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_governor_fails() {
        let err = cfg(Some("nope")).governors().unwrap_err();
        assert!(err.contains("31337"), "{err}");
    }
}
