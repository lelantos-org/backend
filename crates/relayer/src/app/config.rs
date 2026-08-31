use serde::Deserialize;
use std::path::PathBuf;

use crate::adapters::calldata::MAX_DEPOSITS_PER_BATCH;

#[derive(Debug, Deserialize, Clone)]
pub struct RelayerConfig {
    pub database_url: String,
    pub listen_addr: String,
    /// Per-chain settings. The relayer holds one in-memory tree mirror and one
    /// alloy provider per entry.
    pub chains: Vec<ChainCfg>,
    pub prover: ProverCfg,
    #[serde(default)]
    pub price_oracle: PriceOracleCfg,
    #[serde(default)]
    pub token_prices: TokenPricesCfg,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainCfg {
    pub chain_id: i64,
    pub rpc_url: String,
    /// MASP pool address, the target of `transact` calls.
    pub pool_address: String,
    /// Relayer signer key, 32-byte hex. Must match the on-chain `relayer` address
    /// wallets pin in their transact proofs. A `withdrawNative` payload pins
    /// `native_adapter_address` instead, since the adapter is the pool's caller
    /// there.
    pub signer_key_hex: String,
    /// Receipt poll budget in seconds. On a submission revert the in-memory tree
    /// rolls back and the HTTP caller receives a 502.
    #[serde(default = "default_receipt_timeout_s")]
    pub receipt_timeout_s: u64,
    /// Receipt poll interval in milliseconds, driving alloy's pending-transaction
    /// watcher. Roughly a quarter of block time keeps confirmation latency tracking
    /// block production without over-polling the RPC.
    #[serde(default = "default_receipt_poll_interval_ms")]
    pub receipt_poll_interval_ms: u64,
    /// Interval in seconds for the shield flush worker, which polls
    /// `deposit_escrowed_events` for unflushed deposits, batches up to
    /// `flush_max_n`, and submits one `flushBatch` transaction.
    #[serde(default = "default_flush_interval_s")]
    pub flush_interval_s: u64,
    /// Upper bound on per-flush batch size, counted in deposits. A deposit is two
    /// leaves, its own note and the note paying the flusher, so this is capped at
    /// `MAX_L_BATCH / LEAVES_PER_DEPOSIT`.
    #[serde(default = "default_flush_max_n")]
    pub flush_max_n: usize,
    /// How many attributable failures one deposit is allowed before the flush
    /// worker stops batching it. `flushBatch` is all-or-nothing and the oldest
    /// deposits are batched first, so without a cap a single deposit that can never
    /// land blocks every newer one on its chain. `0` disables quarantine. Skipping
    /// is safe: the payer can still reclaim the deposit with `cancelDeposit`.
    #[serde(default = "default_flush_max_attempts")]
    pub flush_max_attempts: u32,
    /// When set, enables `withdrawNative` for this chain: the submitter targets
    /// this address and the SNARK must name it as both `recipient` and `relayer`.
    /// Mirrors `NativeAdapter.sol` deployed alongside MASP, which is ERC-20 only.
    #[serde(default)]
    pub native_adapter_address: Option<String>,
    /// When set, enables `/v1/swap` for this chain. The submitter targets this
    /// address for swap calldata while spends still target `pool_address`. Mirrors
    /// `SwapWrapper.sol` deployed alongside MASP.
    #[serde(default)]
    pub swap_wrapper_address: Option<String>,
    /// Oracle base symbol for the chain's native gas token, such as "ETH" or
    /// "BNB". Used as `base` in Coinbase price lookups.
    #[serde(default = "default_native_symbol")]
    pub native_symbol: String,
    /// Native token decimals; 18 on every EVM chain currently supported.
    #[serde(default = "default_native_decimals")]
    pub native_decimals: u8,
    /// Per-chain markup applied on top of the raw gas cost, in basis points
    /// (1000 = 10%).
    #[serde(default = "default_fee_markup_bps")]
    pub fee_markup_bps: u32,
    /// How long a swap without a wallet-supplied deadline stays valid, in seconds.
    /// The wrapper reverts `SwapExpired` past it. Without a bound, a swap can sit
    /// in the mempool and execute at an arbitrarily later price with only
    /// `min_out` protecting the user.
    #[serde(default = "default_swap_deadline_s")]
    pub swap_default_deadline_s: u64,
    /// Accepted fee tokens for `/v1/spend/estimate` and `/v1/swap/estimate`.
    #[serde(default)]
    pub accepted_fee_tokens: Vec<FeeTokenCfg>,
    /// bech32m shielded payment address the relayer is paid at. Setting it enables
    /// shielded fee collection for this chain: `/chains` publishes the terms, and
    /// every spend and swap must carry an output note to this address covering the
    /// quote. Absent means the relayer subsidises gas.
    #[serde(default)]
    pub shielded_fee_address: Option<String>,
    /// Incoming viewing key for [`Self::shielded_fee_address`], 0x-hex or
    /// decimal, big-endian.
    ///
    /// Decrypt-only: it recognises payments and reads their value but confers no
    /// authority to spend them, so the spending key can stay off this host, which
    /// is exposed to the internet. Normally supplied as
    /// `RELAYER_CHAIN_<id>_SHIELDED_FEE_IVK` rather than written into the TOML.
    #[serde(default)]
    pub shielded_fee_ivk: Option<String>,
    /// How far below the relayer's submit-time quote a fee may fall before it is
    /// refused, in basis points (300 = 3%).
    ///
    /// A quote is unsigned and unstored, so the relayer re-derives the requirement
    /// when the spend arrives. The gas price and the oracle rate both move between
    /// the client's estimate and that moment. A wide band is a discount anyone can
    /// take by waiting, so this is a tolerance rather than a margin.
    #[serde(default = "default_shielded_fee_grace_bps")]
    pub shielded_fee_grace_bps: u32,
    /// MASP asset ids accepted as shielded fees. Empty means every asset in
    /// `accepted_fee_tokens` is accepted.
    ///
    /// The wallet builds one spend in one asset, so a payer can only pay the fee in
    /// the asset they are already moving; an asset left out of this list is one the
    /// relayer will not move at all.
    #[serde(default)]
    pub shielded_fee_assets: Vec<u64>,
    /// Wallet-facing description, served verbatim by `/chains`.
    #[serde(default)]
    pub public: ChainPublicCfg,
}

/// What a wallet needs in order to talk to this chain, and nothing the relayer
/// itself reads.
///
/// It lives here because the relayer is the only service that already enumerates
/// every chain, making it the registry a wallet boots from: a deployment can add
/// a chain without rebuilding any frontend.
///
/// Every field is optional so existing configs keep booting. A client that finds
/// one absent falls back to its own build-time configuration.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ChainPublicCfg {
    /// Human label; also what `wallet_addEthereumChain` registers.
    #[serde(default)]
    pub name: Option<String>,
    /// Browser-reachable RPC.
    ///
    /// Separate from `ChainCfg::rpc_url`, which is the relayer's own endpoint and
    /// is typically cluster-internal; serving that to a browser would hand out an
    /// unreachable URL.
    #[serde(default)]
    pub rpc_url: Option<String>,
    /// Merkle depth of the deployed pool.
    #[serde(default)]
    pub tree_depth: Option<u32>,
    #[serde(default)]
    pub permit2_address: Option<String>,
    /// Block-explorer base, for transaction links.
    #[serde(default)]
    pub explorer_url: Option<String>,
}

/// The `shielded_fee_*` keys, grouped once they are known to be coherent.
///
/// They live flat on [`ChainCfg`] so the `RELAYER_CHAIN_<id>_<FIELD>` overlay can
/// reach the viewing key; a nested table would force the one secret among them
/// into the committed TOML. Callers receive them grouped, since four fields that
/// are only meaningful together should not be usable apart.
#[derive(Debug, Clone, Copy)]
pub struct ShieldedFeeSettings<'a> {
    pub address: &'a str,
    pub ivk: &'a str,
    pub grace_bps: u32,
    /// Empty means every token in `accepted_fee_tokens`.
    pub assets: &'a [u64],
}

impl ChainCfg {
    /// This chain's shielded fee settings, or `None` where it charges nothing.
    ///
    /// `Some` implies both an address and a key: [`RelayerConfig::validate`]
    /// refuses one without the other, so a half-configured chain cannot reach
    /// here.
    pub fn shielded_fee(&self) -> Option<ShieldedFeeSettings<'_>> {
        let address = self.shielded_fee_address.as_deref()?;
        let ivk = self.shielded_fee_ivk.as_deref()?;
        Some(ShieldedFeeSettings {
            address,
            ivk,
            grace_bps: self.shielded_fee_grace_bps,
            assets: &self.shielded_fee_assets,
        })
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct FeeTokenCfg {
    pub symbol: String,
    pub address: String,
    pub decimals: u8,
    /// Oracle quote symbol, for example "USDC".
    pub quote_symbol: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PriceOracleCfg {
    #[serde(default = "default_oracle_base_url")]
    pub base_url: String,
    #[serde(default = "default_oracle_endpoint")]
    pub endpoint: String, // "spot" | "buy"
    /// How long a cached price is served without refetching.
    #[serde(default = "default_oracle_ttl_s")]
    pub cache_ttl_s: u64,
    /// How much further past `cache_ttl_s` a cached price may be served when the
    /// upstream fetch fails. Measured from the end of the TTL rather than from the
    /// fetch.
    #[serde(default = "default_oracle_max_stale_s")]
    pub max_stale_s: u64,
    #[serde(default = "default_oracle_allow_usd_cross")]
    pub allow_usd_cross: bool,
}

impl Default for PriceOracleCfg {
    fn default() -> Self {
        Self {
            base_url: default_oracle_base_url(),
            endpoint: default_oracle_endpoint(),
            cache_ttl_s: default_oracle_ttl_s(),
            max_stale_s: default_oracle_max_stale_s(),
            allow_usd_cross: default_oracle_allow_usd_cross(),
        }
    }
}

/// Spot USD prices for the registered assets, as `/v1/prices` publishes them.
///
/// Distinct from `price_oracle` above. That one prices a fee from a symbol pair
/// and a failure there fails a submission; this one prices a token from its
/// address so a wallet can label a balance, and a failure here only omits a
/// label. Different keys, provider and consequence.
#[derive(Debug, Deserialize, Clone)]
pub struct TokenPricesCfg {
    /// DefiLlama-compatible price API root.
    #[serde(default = "default_token_price_base_url")]
    pub base_url: String,
    /// How long a spot price is served without refetching.
    #[serde(default = "default_token_price_ttl_s")]
    pub ttl_s: u64,
    /// Upstream deadline. Prices are decoration, so a slow provider must not hold
    /// the endpoint open.
    #[serde(default = "default_token_price_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for TokenPricesCfg {
    fn default() -> Self {
        Self {
            base_url: default_token_price_base_url(),
            ttl_s: default_token_price_ttl_s(),
            timeout_ms: default_token_price_timeout_ms(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProverCfg {
    /// Path to `circuits/build/tree_update_batch.wcd`, the witness-calculation
    /// graph `just build-graph` emits.
    ///
    /// Replaces the former `wasm_path` / `r1cs_path` pair. Both are now ignored
    /// if still present in a deployed `relayer.toml`, but this key is required:
    /// the new binary cannot prove without the graph artifact, so the config and
    /// the `/circuits` mount have to roll out together either way.
    pub graph_path: PathBuf,
    /// Path to `circuits/build/tree_update_final.zkey`, snarkjs-compatible.
    pub zkey_path: PathBuf,
    /// snarkjs `verification_key.json` for the deployed transact circuit.
    ///
    /// Optional so a deployment that has not shipped the artifact still boots, but
    /// it should be set: without it the relayer cannot distinguish a valid wallet
    /// proof from a fabricated one until the contract does, so every invalid
    /// payload costs a full `tree_update_batch` Groth16 first.
    #[serde(default)]
    pub transact_vkey_path: Option<PathBuf>,
}

fn default_receipt_timeout_s() -> u64 {
    60
}

fn default_receipt_poll_interval_ms() -> u64 {
    250
}

fn default_flush_interval_s() -> u64 {
    30
}

fn default_flush_max_n() -> usize {
    // Derived, not restated: `state.rs` already clamps to this figure, so a
    // literal here silently caps every flush at the old batch width the moment
    // `MAX_L_BATCH` moves — which is exactly what happened when it widened to 8
    // and this default stayed at the pre-widening 2.
    MAX_DEPOSITS_PER_BATCH
}

fn default_flush_max_attempts() -> u32 {
    5
}

fn default_native_symbol() -> String {
    "ETH".to_string()
}

fn default_native_decimals() -> u8 {
    18
}

fn default_fee_markup_bps() -> u32 {
    1000
}

fn default_swap_deadline_s() -> u64 {
    300
}

fn default_oracle_base_url() -> String {
    "https://api.coinbase.com/v2".to_string()
}

fn default_oracle_endpoint() -> String {
    "spot".to_string()
}

fn default_oracle_ttl_s() -> u64 {
    300
}

fn default_oracle_max_stale_s() -> u64 {
    300
}

fn default_oracle_allow_usd_cross() -> bool {
    true
}

fn default_token_price_base_url() -> String {
    "https://coins.llama.fi".to_string()
}

fn default_token_price_ttl_s() -> u64 {
    300
}

fn default_token_price_timeout_ms() -> u64 {
    5_000
}

fn default_shielded_fee_grace_bps() -> u32 {
    300
}

/// The widest exponent `10u128.pow(n)` accepts.
const MAX_DECIMALS: u8 = 38;
/// A 100x markup. Anything above this is a typo, and `10_000 + bps` must not
/// overflow.
const MAX_MARKUP_BPS: u32 = 1_000_000;
/// One whole in basis points.
pub const BPS_DENOMINATOR: u32 = 10_000;

/// Everything wrong with a config, rendered as one message.
#[derive(Debug)]
pub struct ConfigErrors(Vec<String>);

impl std::fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} problem(s):", self.0.len())?;
        for p in &self.0 {
            write!(f, "\n  - {p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

impl ChainCfg {
    /// Everything wrong with this chain's settings.
    fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut check = |ok: bool, msg: String| {
            if !ok {
                out.push(format!("chain {}: {msg}", self.chain_id));
            }
        };
        check(
            self.native_decimals <= MAX_DECIMALS,
            format!(
                "native_decimals {} exceeds {MAX_DECIMALS}",
                self.native_decimals
            ),
        );
        for t in &self.accepted_fee_tokens {
            check(
                t.decimals <= MAX_DECIMALS,
                format!(
                    "fee token {} decimals {} exceeds {MAX_DECIMALS}",
                    t.symbol, t.decimals
                ),
            );
        }
        check(
            self.fee_markup_bps <= MAX_MARKUP_BPS,
            format!(
                "fee_markup_bps {} exceeds {MAX_MARKUP_BPS}",
                self.fee_markup_bps
            ),
        );
        check(
            self.flush_interval_s > 0,
            "flush_interval_s must be > 0".to_string(),
        );
        check(
            self.shielded_fee_grace_bps < BPS_DENOMINATOR,
            format!(
                "shielded_fee_grace_bps {} must be below {BPS_DENOMINATOR}; at or above it every \
                 fee, including none at all, clears the check",
                self.shielded_fee_grace_bps
            ),
        );
        // An address without the key rejects every spend, and a key without an
        // address collects nothing. Neither is visible at runtime, so both are
        // fatal here.
        check(
            self.shielded_fee_address.is_some() == self.shielded_fee_ivk.is_some(),
            "shielded_fee_address and shielded_fee_ivk must be set together".to_string(),
        );
        check(
            self.shielded_fee_address.is_some() || self.shielded_fee_assets.is_empty(),
            "shielded_fee_assets is set but shielded_fee_address is not, so no fee is collected"
                .to_string(),
        );
        out
    }
}

impl RelayerConfig {
    /// Boot-time checks on values the rest of the code assumes are sound.
    ///
    /// Each would otherwise fail at runtime far from its cause: a duplicate chain
    /// runs two independent tree mirrors against one chain, and the decimal and
    /// markup bounds guard arithmetic that panics rather than erroring.
    ///
    /// Every problem is reported rather than only the first, so an operator fixing
    /// a config does not restart once per mistake.
    pub fn validate(&self) -> Result<(), ConfigErrors> {
        let mut problems = Vec::new();
        if self.chains.is_empty() {
            problems.push("no chains configured".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.chains {
            if !seen.insert(c.chain_id) {
                problems.push(format!(
                    "chain_id {} is declared more than once; each chain owns one tree mirror \
                     and one flush worker, so a duplicate desyncs both",
                    c.chain_id
                ));
            }
            problems.extend(c.problems());
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigErrors(problems))
        }
    }

    /// Overlay env vars on top of the TOML defaults, per chain, using the
    /// convention `RELAYER_CHAIN_<id>_<FIELD>`, for example:
    ///   RELAYER_CHAIN_<id>_POOL_ADDRESS=0x…
    ///   RELAYER_CHAIN_<id>_RPC_URL=http://…
    ///   RELAYER_CHAIN_<id>_SIGNER_KEY=0x…
    ///   RELAYER_CHAIN_<id>_SHIELDED_FEE_IVK=0x…
    ///
    /// # Panics
    ///
    /// If `ACCEPTED_FEE_TOKENS` is set to something that is not a JSON array of
    /// fee-token records.
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
            if let Some(v) = shared::config_env::lookup("RELAYER", c.chain_id, "FEE_MARKUP_BPS")
                && let Ok(n) = v.parse::<u32>()
            {
                c.fee_markup_bps = n;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(chain_id: i64) -> ChainCfg {
        ChainCfg {
            chain_id,
            rpc_url: "http://localhost:8545".into(),
            pool_address: "0x0000000000000000000000000000000000000001".into(),
            signer_key_hex: "0x01".into(),
            receipt_timeout_s: default_receipt_timeout_s(),
            receipt_poll_interval_ms: default_receipt_poll_interval_ms(),
            flush_interval_s: default_flush_interval_s(),
            flush_max_n: default_flush_max_n(),
            flush_max_attempts: default_flush_max_attempts(),
            native_adapter_address: None,
            swap_wrapper_address: None,
            native_symbol: default_native_symbol(),
            native_decimals: default_native_decimals(),
            fee_markup_bps: default_fee_markup_bps(),
            swap_default_deadline_s: default_swap_deadline_s(),
            accepted_fee_tokens: vec![],
            shielded_fee_address: None,
            shielded_fee_ivk: None,
            shielded_fee_grace_bps: default_shielded_fee_grace_bps(),
            shielded_fee_assets: vec![],
            public: ChainPublicCfg::default(),
        }
    }

    #[test]
    fn a_shielded_fee_address_without_its_viewing_key_is_refused() {
        let mut c = chain(1);
        c.shielded_fee_address = Some("lelantos1abc".into());
        let err = cfg(vec![c]).validate().expect_err("half a config");
        assert!(err.to_string().contains("must be set together"), "{err}");
    }

    #[test]
    fn a_viewing_key_without_an_address_is_refused() {
        let mut c = chain(1);
        c.shielded_fee_ivk = Some("0x01".into());
        assert!(cfg(vec![c]).validate().is_err());
    }

    /// A 100% grace band clears any fee, including none, so it looks like
    /// enforcement without being it.
    #[test]
    fn a_grace_band_of_a_whole_is_refused() {
        let mut c = chain(1);
        c.shielded_fee_grace_bps = BPS_DENOMINATOR;
        let err = cfg(vec![c]).validate().expect_err("grace of 100%");
        assert!(err.to_string().contains("shielded_fee_grace_bps"), "{err}");
    }

    #[test]
    fn accepted_assets_without_an_address_collect_nothing_and_are_refused() {
        let mut c = chain(1);
        c.shielded_fee_assets = vec![1];
        assert!(cfg(vec![c]).validate().is_err());
    }

    #[test]
    fn a_complete_shielded_fee_block_validates() {
        let mut c = chain(1);
        c.shielded_fee_address = Some("lelantos1abc".into());
        c.shielded_fee_ivk = Some("0x01".into());
        c.shielded_fee_assets = vec![1, 3];
        assert!(cfg(vec![c]).validate().is_ok());
    }

    fn cfg(chains: Vec<ChainCfg>) -> RelayerConfig {
        RelayerConfig {
            database_url: "postgres://localhost/x".into(),
            listen_addr: "0.0.0.0:3003".into(),
            chains,
            prover: ProverCfg {
                graph_path: "/g".into(),
                zkey_path: "/z".into(),
                transact_vkey_path: None,
            },
            price_oracle: PriceOracleCfg::default(),
            token_prices: TokenPricesCfg::default(),
        }
    }

    /// The shipped TOMLs declare no `[token_prices]` section, so every field comes
    /// from a serde default. A field losing its `#[serde(default)]` would stop the
    /// relayer booting, and only in a deployment, since nothing else here parses a
    /// real config file.
    #[test]
    fn the_shipped_configs_still_parse_without_a_token_prices_section() {
        for path in [
            "../../stack/config/dev/relayer.toml",
            "../../stack/config/prod/relayer.toml",
        ] {
            let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert!(
                !raw.contains("[token_prices]"),
                "{path} now sets the section; this test no longer proves the defaults work",
            );
            let cfg: RelayerConfig = toml::from_str(&raw).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(cfg.token_prices.base_url, "https://coins.llama.fi");
            assert_eq!(cfg.token_prices.ttl_s, 300);
            assert_eq!(cfg.token_prices.timeout_ms, 5_000);
        }
    }

    #[test]
    fn a_well_formed_config_validates() {
        cfg(vec![chain(1), chain(2)]).validate().unwrap();
    }

    /// A duplicate would build two independent `TreeMirror`s and two flush workers
    /// for one chain, which desyncs rather than merely misconfigures.
    #[test]
    fn a_duplicate_chain_id_is_refused() {
        let err = cfg(vec![chain(1), chain(1)]).validate().unwrap_err();
        assert!(err.to_string().contains("more than once"), "got {err}");
    }

    /// An operator fixing a config sees every mistake at once.
    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let mut c = chain(1);
        c.native_decimals = 39;
        c.fee_markup_bps = u32::MAX;
        c.flush_interval_s = 0;

        let err = cfg(vec![c]).validate().unwrap_err().to_string();

        assert!(err.contains("native_decimals"), "got {err}");
        assert!(err.contains("fee_markup_bps"), "got {err}");
        assert!(err.contains("flush_interval_s"), "got {err}");
    }

    #[test]
    fn decimals_that_would_overflow_the_fee_math_are_refused() {
        let mut c = chain(1);
        c.native_decimals = 39;
        assert!(cfg(vec![c]).validate().is_err());

        let mut c = chain(1);
        c.accepted_fee_tokens.push(FeeTokenCfg {
            symbol: "X".into(),
            address: "0x0000000000000000000000000000000000000002".into(),
            decimals: 77,
            quote_symbol: "USDC".into(),
        });
        assert!(cfg(vec![c]).validate().is_err());
    }

    #[test]
    fn an_absurd_markup_is_refused() {
        let mut c = chain(1);
        c.fee_markup_bps = u32::MAX;
        assert!(cfg(vec![c]).validate().is_err());
    }

    #[test]
    fn a_zero_flush_interval_is_refused() {
        let mut c = chain(1);
        c.flush_interval_s = 0;
        assert!(cfg(vec![c]).validate().is_err());
    }

    /// The overlay lets a deploy script inject ERC-20 addresses it only learns at
    /// deploy time. A chain id no other test touches keeps the process-wide env
    /// mutation from reaching them.
    ///
    /// SAFETY: the mutations are scoped to a chain id used nowhere else, so no
    /// concurrent test observes them.
    #[test]
    fn test_accepted_fee_tokens_env_overlay_replaces_the_toml_list() {
        const CHAIN: i64 = 987_654;
        let key = format!("RELAYER_CHAIN_{CHAIN}_ACCEPTED_FEE_TOKENS");
        unsafe {
            std::env::set_var(
                &key,
                r#"[{"symbol":"USDC","address":"0xabc","decimals":6,"quote_symbol":"USD"}]"#,
            );
        }

        let mut config = cfg(vec![ChainCfg {
            accepted_fee_tokens: vec![FeeTokenCfg {
                symbol: "STALE".into(),
                address: "0xstale".into(),
                decimals: 18,
                quote_symbol: "USD".into(),
            }],
            ..chain(CHAIN)
        }]);
        config.apply_env_overlay();

        unsafe { std::env::remove_var(&key) };

        let tokens = &config.chains[0].accepted_fee_tokens;
        assert_eq!(
            tokens.len(),
            1,
            "the TOML entry must be replaced, not merged"
        );
        assert_eq!(tokens[0].symbol, "USDC");
        assert_eq!(tokens[0].address, "0xabc");
        assert_eq!(tokens[0].decimals, 6);
    }

    /// Absent means keep what the TOML declared: the overlay is optional, and a
    /// deployment configuring fee tokens statically must keep working.
    #[test]
    fn test_accepted_fee_tokens_without_the_env_var_keeps_the_toml_list() {
        const CHAIN: i64 = 987_655;
        unsafe { std::env::remove_var(format!("RELAYER_CHAIN_{CHAIN}_ACCEPTED_FEE_TOKENS")) };

        let mut config = cfg(vec![ChainCfg {
            accepted_fee_tokens: vec![FeeTokenCfg {
                symbol: "KEEP".into(),
                address: "0xkeep".into(),
                decimals: 18,
                quote_symbol: "USD".into(),
            }],
            ..chain(CHAIN)
        }]);
        config.apply_env_overlay();

        assert_eq!(config.chains[0].accepted_fee_tokens[0].symbol, "KEEP");
    }
}
