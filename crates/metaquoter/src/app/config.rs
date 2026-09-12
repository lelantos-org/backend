//! `metaquoter.toml` plus the per-chain `METAQUOTER_CHAIN_<id>_*` env overlay.

use crate::domain::error::{AppError, AppResult};
use crate::domain::fees::BPS_DENOMINATOR;
use alloy::primitives::Address;
use serde::Deserialize;
use shared::config_env::{self, ParseError};
use std::collections::HashSet;
use std::str::FromStr;

#[derive(Debug, Deserialize, Clone)]
pub struct MetaQuoterConfig {
    pub listen_addr: String,
    /// Per-chain RPC + venue addresses. Must be non-empty.
    pub chains: Vec<ChainCfg>,
    /// Per-quoter race deadline in milliseconds. Slower quoters are dropped from
    /// the race rather than failing the request.
    #[serde(default = "default_race_deadline_ms")]
    pub race_deadline_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainCfg {
    pub chain_id: u64,
    pub rpc_url: String,
    /// UniV3 QuoterV2 address.
    pub univ3_quoter: Address,
    /// Deployed `UniV3Adapter` address, returned to the SDK to identify which
    /// `ISwapAdapter` the route is bound to.
    pub univ3_adapter: Address,
    /// UniV4 `V4Quoter` address. Optional: a chain is quoted on V4 only when
    /// both this and `univ4_adapter` are set, so an existing V3-only config
    /// keeps parsing and keeps behaving exactly as before.
    #[serde(default)]
    pub univ4_quoter: Option<Address>,
    /// Deployed `UniV4Adapter` address. See `univ4_quoter`.
    #[serde(default)]
    pub univ4_adapter: Option<Address>,
    /// MASP wrapper fee on `amount_out`, in basis points, deducted from the
    /// venue's gross output before slippage. Zero disables the fee.
    #[serde(default)]
    pub masp_fee_bps: u16,
}

fn default_race_deadline_ms() -> u64 {
    1_500
}

impl MetaQuoterConfig {
    /// Overlay env vars on top of the TOML defaults, per chain, using the same
    /// convention as the relayer:
    ///   METAQUOTER_CHAIN_<id>_RPC_URL=http://…
    ///   METAQUOTER_CHAIN_<id>_UNIV3_QUOTER=0x…
    ///   METAQUOTER_CHAIN_<id>_UNIV3_ADAPTER=0x…
    ///   METAQUOTER_CHAIN_<id>_UNIV4_QUOTER=0x…
    ///   METAQUOTER_CHAIN_<id>_UNIV4_ADAPTER=0x…
    ///   METAQUOTER_CHAIN_<id>_MASP_FEE_BPS=25
    ///
    /// A variable that is set but does not parse fails startup rather than
    /// falling back to the TOML value. These name the deployed adapter a route
    /// binds to; a typo'd one that quietly kept the old address would emit
    /// quotes pointing at the wrong `ISwapAdapter` with nothing in the log to
    /// say so.
    ///
    /// The overlay only rewrites chains already declared in the TOML. A variable
    /// naming a chain with no `[[chains]]` block is discarded.
    pub fn apply_env_overlay(&mut self) -> Result<(), ParseError> {
        for c in &mut self.chains {
            let id = c.chain_id as i64;
            if let Some(url) = config_env::lookup("METAQUOTER", id, "RPC_URL") {
                c.rpc_url = url;
            }
            overlay(id, "UNIV3_QUOTER", &mut c.univ3_quoter)?;
            overlay(id, "UNIV3_ADAPTER", &mut c.univ3_adapter)?;
            overlay_opt(id, "UNIV4_QUOTER", &mut c.univ4_quoter)?;
            overlay_opt(id, "UNIV4_ADAPTER", &mut c.univ4_adapter)?;
            overlay(id, "MASP_FEE_BPS", &mut c.masp_fee_bps)?;
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

        // Two `[[chains]]` blocks for one id used to be last-wins through a
        // `HashMap::insert`, so a stale block left below a corrected one
        // silently decided which adapter every quote for that chain bound to.
        let mut seen = HashSet::new();
        for c in &self.chains {
            if !seen.insert(c.chain_id) {
                return Err(AppError::Internal(format!(
                    "chain {} is declared more than once",
                    c.chain_id
                )));
            }
            // A fee at or above the denominator makes `max_deposit` return an
            // amount unrelated to the venue's output: a mistyped unit, not a
            // configuration.
            if c.masp_fee_bps >= BPS_DENOMINATOR {
                return Err(AppError::Internal(format!(
                    "chain {}: masp_fee_bps {} must be below {BPS_DENOMINATOR}",
                    c.chain_id, c.masp_fee_bps
                )));
            }
        }
        Ok(())
    }
}

/// Overlay one required field. Unset leaves the TOML value; set-but-malformed is
/// an error, never a silent fallback.
fn overlay<T: FromStr>(chain_id: i64, field: &str, slot: &mut T) -> Result<(), ParseError> {
    if let Some(v) = config_env::lookup_parse::<T>("METAQUOTER", chain_id, field)? {
        *slot = v;
    }
    Ok(())
}

/// [`overlay`] for an optional slot: the overlay can fill a key in but never
/// clear one, since an unset variable is how a V3-only chain is expressed.
fn overlay_opt<T: FromStr>(
    chain_id: i64,
    field: &str,
    slot: &mut Option<T>,
) -> Result<(), ParseError> {
    if let Some(v) = config_env::lookup_parse::<T>("METAQUOTER", chain_id, field)? {
        *slot = Some(v);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const TOML_QUOTER: Address = address!("1111111111111111111111111111111111111111");
    const TOML_ADAPTER: Address = address!("2222222222222222222222222222222222222222");
    const ENV_ADAPTER: &str = "0x3333333333333333333333333333333333333333";

    /// Scopes a variable to one test. Every key below is unique to its test, so
    /// the mutations never race even though the harness runs tests in parallel,
    /// which is what makes the `unsafe` sound.
    struct EnvVar(&'static str);

    impl EnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: this key is used by no other test.
            unsafe { std::env::set_var(key, value) };
            Self(key)
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe { std::env::remove_var(self.0) };
        }
    }

    fn chain(chain_id: u64) -> ChainCfg {
        ChainCfg {
            chain_id,
            rpc_url: "http://toml:8545".into(),
            univ3_quoter: TOML_QUOTER,
            univ3_adapter: TOML_ADAPTER,
            univ4_quoter: None,
            univ4_adapter: None,
            masp_fee_bps: 0,
        }
    }

    fn cfg(chains: Vec<ChainCfg>) -> MetaQuoterConfig {
        MetaQuoterConfig {
            listen_addr: "0.0.0.0:8081".into(),
            chains,
            race_deadline_ms: 1_500,
        }
    }

    #[test]
    fn overlay_replaces_a_declared_chains_fields() {
        let _a = EnvVar::set("METAQUOTER_CHAIN_777001_UNIV3_ADAPTER", ENV_ADAPTER);
        let _r = EnvVar::set("METAQUOTER_CHAIN_777001_RPC_URL", "http://env:8545");

        let mut c = cfg(vec![chain(777001)]);
        c.apply_env_overlay().unwrap();

        assert_eq!(
            c.chains[0].univ3_adapter,
            ENV_ADAPTER.parse::<Address>().unwrap()
        );
        assert_eq!(c.chains[0].rpc_url, "http://env:8545");
        // Untouched fields keep the TOML value.
        assert_eq!(c.chains[0].univ3_quoter, TOML_QUOTER);
    }

    /// The overlay fills a V4 pair in; it must never clear one, because unset is
    /// how a V3-only chain is written.
    #[test]
    fn overlay_fills_an_optional_field_and_leaves_an_unset_one_alone() {
        let _q = EnvVar::set("METAQUOTER_CHAIN_777002_UNIV4_QUOTER", ENV_ADAPTER);

        let mut c = cfg(vec![chain(777002)]);
        c.apply_env_overlay().unwrap();

        assert_eq!(
            c.chains[0].univ4_quoter,
            Some(ENV_ADAPTER.parse::<Address>().unwrap())
        );
        assert_eq!(c.chains[0].univ4_adapter, None);
    }

    /// A truncated or mistyped address named the adapter every quote for that
    /// chain binds to. Falling back to the TOML value would serve routes against
    /// a different contract than the operator believes is configured.
    #[test]
    fn a_malformed_address_fails_startup_rather_than_falling_back() {
        let _a = EnvVar::set("METAQUOTER_CHAIN_777003_UNIV3_ADAPTER", "0xdeadbeef");

        let mut c = cfg(vec![chain(777003)]);
        let err = c.apply_env_overlay().unwrap_err();

        assert!(
            err.to_string()
                .contains("METAQUOTER_CHAIN_777003_UNIV3_ADAPTER"),
            "{err}"
        );
        assert!(err.to_string().contains("0xdeadbeef"), "{err}");
    }

    /// Compose renders an unset substitution as the empty string; honouring it
    /// would blank a good TOML value.
    #[test]
    fn an_empty_variable_reads_as_unset() {
        let _a = EnvVar::set("METAQUOTER_CHAIN_777004_UNIV3_ADAPTER", "");
        let _r = EnvVar::set("METAQUOTER_CHAIN_777004_RPC_URL", "");

        let mut c = cfg(vec![chain(777004)]);
        c.apply_env_overlay().unwrap();

        assert_eq!(c.chains[0].univ3_adapter, TOML_ADAPTER);
        assert_eq!(c.chains[0].rpc_url, "http://toml:8545");
    }

    #[test]
    fn a_duplicate_chain_block_is_rejected() {
        let err = cfg(vec![chain(1), chain(1)]).validate().unwrap_err();
        assert!(err.to_string().contains("declared more than once"), "{err}");
    }

    #[test]
    fn an_empty_chain_list_is_rejected() {
        assert!(cfg(vec![]).validate().is_err());
    }

    /// A fee at or above 100% makes `max_deposit` return an amount unrelated to
    /// the venue's output; it is a mistyped unit, not a configuration.
    #[test]
    fn a_fee_at_or_above_the_denominator_is_rejected() {
        let mut c = chain(1);
        c.masp_fee_bps = 10_000;
        assert!(cfg(vec![c]).validate().is_err());

        let mut ok = chain(1);
        ok.masp_fee_bps = 9_999;
        assert!(cfg(vec![ok]).validate().is_ok());
    }
}
