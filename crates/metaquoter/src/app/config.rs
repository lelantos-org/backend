use alloy::primitives::Address;
use serde::Deserialize;
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
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            overlay_str(c.chain_id, "RPC_URL", &mut c.rpc_url);
            overlay_parsed::<Address>(c.chain_id, "UNIV3_QUOTER", &mut c.univ3_quoter);
            overlay_parsed::<Address>(c.chain_id, "UNIV3_ADAPTER", &mut c.univ3_adapter);
            overlay_parsed_opt::<Address>(c.chain_id, "UNIV4_QUOTER", &mut c.univ4_quoter);
            overlay_parsed_opt::<Address>(c.chain_id, "UNIV4_ADAPTER", &mut c.univ4_adapter);
            overlay_parsed::<u16>(c.chain_id, "MASP_FEE_BPS", &mut c.masp_fee_bps);
        }
    }
}

fn overlay_str(chain_id: u64, field: &str, slot: &mut String) {
    if let Some(v) = shared::config_env::lookup("METAQUOTER", chain_id as i64, field) {
        *slot = v;
    }
}

/// Same as [`overlay_parsed`] for an optional slot: an unset or unparseable
/// variable leaves whatever the TOML declared, so the overlay can fill a key in
/// but never clear one.
fn overlay_parsed_opt<T: FromStr>(chain_id: u64, field: &str, slot: &mut Option<T>) {
    if let Some(parsed) = shared::config_env::lookup("METAQUOTER", chain_id as i64, field)
        .and_then(|v| v.parse::<T>().ok())
    {
        *slot = Some(parsed);
    }
}

fn overlay_parsed<T: FromStr>(chain_id: u64, field: &str, slot: &mut T) {
    if let Some(parsed) = shared::config_env::lookup("METAQUOTER", chain_id as i64, field)
        .and_then(|v| v.parse::<T>().ok())
    {
        *slot = parsed;
    }
}
