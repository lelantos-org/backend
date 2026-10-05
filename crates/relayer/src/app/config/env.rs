//! The per-chain env overlay, applied on top of the TOML before validation.

use super::RelayerConfig;
use serde::de::DeserializeOwned;

impl RelayerConfig {
    /// Overlay env vars on top of the TOML defaults, per chain, using the
    /// convention `RELAYER_CHAIN_<id>_<FIELD>`, for example:
    ///
    /// ```text
    /// RELAYER_CHAIN_<id>_POOL_ADDRESS=0x…
    /// RELAYER_CHAIN_<id>_RPC_URL=http://…
    /// RELAYER_CHAIN_<id>_SIGNER_KEY=0x…
    /// RELAYER_CHAIN_<id>_SHIELDED_FEE_IVK=0x…
    /// ```
    ///
    /// # Panics
    ///
    /// If `ACCEPTED_FEE_TOKENS` or `GENERIC_ALLOWED_CALLS` is set to something
    /// that is not a JSON array of its records, or a numeric field to something
    /// that does not parse; see `overlay_json_list` and `overlay_parse`.
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "POOL_ADDRESS") {
                c.pool_address = v;
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "RPC_URL") {
                c.rpc_url = v;
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "SIGNER_KEY") {
                c.signer_key_hex = v;
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "BUNDLER_ADDRESS") {
                c.bundler_address = v;
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "REFUND_ADDRESS") {
                c.refund_address = Some(v);
            }
            overlay_parse(&mut c.bundle_max_items, c.chain_id, "BUNDLE_MAX_ITEMS");
            if let Some(v) =
                shared::config_env::lookup("RELAYER", c.chain_id, "SWAP_WRAPPER_ADDRESS")
            {
                c.swap_wrapper_address = Some(v);
            }
            if let Some(v) =
                shared::config_env::lookup("RELAYER", c.chain_id, "GENERIC_CALL_WRAPPER_ADDRESS")
            {
                c.generic_call_wrapper_address = Some(v);
            }
            overlay_json_list(
                &mut c.generic_allowed_calls,
                c.chain_id,
                "GENERIC_ALLOWED_CALLS",
                "{target,selector}",
            );
            if let Some(v) =
                shared::config_env::lookup("RELAYER", c.chain_id, "NATIVE_ADAPTER_ADDRESS")
            {
                c.native_adapter_address = Some(v);
            }
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "NATIVE_SYMBOL") {
                c.native_symbol = v;
            }
            overlay_parse(&mut c.fee_markup_bps, c.chain_id, "FEE_MARKUP_BPS");
            if let Some(v) =
                shared::config_env::lookup("RELAYER", c.chain_id, "SHIELDED_FEE_ADDRESS")
            {
                c.shielded_fee_address = Some(v);
            }
            // The one secret among these. Kept flat on `ChainCfg` so this overlay
            // reaches it; a nested table would force the key into the committed
            // TOML.
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "SHIELDED_FEE_IVK") {
                c.shielded_fee_ivk = Some(v);
            }
            // Deployments learn their ERC-20 addresses from a deploy script, so
            // without this the per-chain config carrying them would have to be
            // written into the committed TOML.
            overlay_json_list(
                &mut c.accepted_fee_tokens,
                c.chain_id,
                "ACCEPTED_FEE_TOKENS",
                "{symbol,address,decimals,quote_symbol}",
            );
        }
    }
}

/// Overlay `RELAYER_CHAIN_<chain_id>_<field>`, a JSON array of `shape` records,
/// onto `target` when set, replacing the TOML's list wholesale.
///
/// # Panics
///
/// If the variable is set but is not such an array. Keeping the TOML's list
/// would run the chain on entries nobody chose.
fn overlay_json_list<T: DeserializeOwned>(
    target: &mut Vec<T>,
    chain_id: i64,
    field: &str,
    shape: &str,
) {
    let Some(raw) = shared::config_env::lookup("RELAYER", chain_id, field) else {
        return;
    };
    match serde_json::from_str(&raw) {
        Ok(list) => *target = list,
        Err(e) => panic!("RELAYER_CHAIN_{chain_id}_{field} is not a JSON array of {shape}: {e}"),
    }
}

/// Overlay `RELAYER_CHAIN_<chain_id>_<field>` onto `target` when set.
///
/// # Panics
///
/// If the variable is set but does not parse. Silently keeping the TOML's value
/// would run the chain on a setting nobody chose.
fn overlay_parse<T: std::str::FromStr>(target: &mut T, chain_id: i64, field: &str) {
    match shared::config_env::lookup_parse::<T>("RELAYER", chain_id, field) {
        Ok(Some(v)) => *target = v,
        Ok(None) => {}
        Err(e) => panic!("chain {chain_id}: {e}"),
    }
}
