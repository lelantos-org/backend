use alloy::primitives::Address;
use serde::Deserialize;
use std::str::FromStr;

#[derive(Debug, Deserialize, Clone)]
pub struct MetaQuoterConfig {
    pub listen_addr: String,
    /// Per-chain RPC + venue addresses. Must be non-empty.
    pub chains: Vec<ChainCfg>,
    /// Per-quoter race deadline (ms). Quoters slower than this are dropped
    /// from the race rather than failing the whole request.
    #[serde(default = "default_race_deadline_ms")]
    pub race_deadline_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainCfg {
    pub chain_id: u64,
    pub rpc_url: String,
    /// UniV3 QuoterV2 address.
    pub univ3_quoter: Address,
    /// Deployed `UniV3Adapter` address — returned to the SDK so it knows
    /// which `ISwapAdapter` the route is bound to.
    pub univ3_adapter: Address,
    /// MASP wrapper fee on `amount_out`, in basis points. Deducted from the
    /// venue's gross output before slippage. 0 = no fee.
    #[serde(default)]
    pub masp_fee_bps: u16,
}

fn default_race_deadline_ms() -> u64 {
    1_500
}

impl MetaQuoterConfig {
    /// Overlay env vars on top of TOML defaults, per chain. Same convention
    /// as the relayer:
    ///   METAQUOTER_CHAIN_<id>_RPC_URL=http://…
    ///   METAQUOTER_CHAIN_<id>_UNIV3_QUOTER=0x…
    ///   METAQUOTER_CHAIN_<id>_UNIV3_ADAPTER=0x…
    ///   METAQUOTER_CHAIN_<id>_MASP_FEE_BPS=25
    pub fn apply_env_overlay(&mut self) {
        for c in &mut self.chains {
            overlay_str(c.chain_id, "RPC_URL", &mut c.rpc_url);
            overlay_parsed::<Address>(c.chain_id, "UNIV3_QUOTER", &mut c.univ3_quoter);
            overlay_parsed::<Address>(c.chain_id, "UNIV3_ADAPTER", &mut c.univ3_adapter);
            overlay_parsed::<u16>(c.chain_id, "MASP_FEE_BPS", &mut c.masp_fee_bps);
        }
    }
}

fn overlay_str(chain_id: u64, field: &str, slot: &mut String) {
    if let Some(v) = shared::config_env::lookup("METAQUOTER", chain_id as i64, field) {
        *slot = v;
    }
}

fn overlay_parsed<T: FromStr>(chain_id: u64, field: &str, slot: &mut T) {
    if let Some(parsed) = shared::config_env::lookup("METAQUOTER", chain_id as i64, field)
        .and_then(|v| v.parse::<T>().ok())
    {
        *slot = parsed;
    }
}
