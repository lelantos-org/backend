//! The per-chain env overlay, applied on top of the TOML before validation.

use super::{FeeTokenCfg, RelayerConfig};

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
    /// If `ACCEPTED_FEE_TOKENS` is set to something that is not a JSON array of
    /// fee-token records, or a numeric field to something that does not parse;
    /// see `overlay_parse`.
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
            // JSON, because this is a list of records while every other overlay is
            // a scalar. Deployments learn their ERC-20 addresses from a deploy
            // script, so without this the per-chain config carrying them would have
            // to be written into the committed TOML.
            //
            // Malformed JSON is a hard failure: keeping the TOML's list would leave
            // the relayer quoting fees against whatever addresses were compiled
            // in.
            if let Some(v) =
                shared::config_env::lookup("RELAYER", c.chain_id, "ACCEPTED_FEE_TOKENS")
            {
                match serde_json::from_str::<Vec<FeeTokenCfg>>(&v) {
                    Ok(tokens) => c.accepted_fee_tokens = tokens,
                    Err(e) => panic!(
                        "RELAYER_CHAIN_{}_ACCEPTED_FEE_TOKENS is not a JSON array of \
                         {{symbol,address,decimals,quote_symbol}}: {e}",
                        c.chain_id
                    ),
                }
            }
        }
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
