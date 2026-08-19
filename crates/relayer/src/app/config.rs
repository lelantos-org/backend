use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct RelayerConfig {
    pub database_url: String,
    pub listen_addr: String,
    /// Per-chain settings; relayer holds one in-memory tree mirror + one
    /// alloy provider per entry.
    pub chains: Vec<ChainCfg>,
    pub prover: ProverCfg,
    #[serde(default)]
    pub price_oracle: PriceOracleCfg,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainCfg {
    pub chain_id: i64,
    pub rpc_url: String,
    /// MASP pool address (target of `transact` calls).
    pub pool_address: String,
    /// Relayer signer key (32-byte hex). MUST match the on-chain bound
    /// `relayer` address that wallets pin in their transact proofs. A
    /// `withdrawNative` payload pins `native_adapter_address` instead — the
    /// adapter is the pool's caller there.
    pub signer_key_hex: String,
    /// Receipt poll budget in seconds. Submission revert ⇒ in-memory tree
    /// rolls back two leaves and the HTTP caller gets 502.
    #[serde(default = "default_receipt_timeout_s")]
    pub receipt_timeout_s: u64,
    /// Receipt poll interval (ms). Drives alloy's pending-tx watcher cadence.
    /// Pick ~1/4 of block time so confirmation latency tracks block production
    /// without hammering the RPC.
    #[serde(default = "default_receipt_poll_interval_ms")]
    pub receipt_poll_interval_ms: u64,
    /// Cron interval (seconds) for the shield flush worker. The worker
    /// polls `deposit_escrowed_events` for unflushed deposits, batches up
    /// to `flush_max_n`, and submits one `flushBatch` tx.
    #[serde(default = "default_flush_interval_s")]
    pub flush_interval_s: u64,
    /// Upper bound on per-flush batch size, counted in deposits. A deposit
    /// is one leaf, so this is capped at the contract's `MAX_L_BATCH = 8`.
    #[serde(default = "default_flush_max_n")]
    pub flush_max_n: usize,
    /// How many attributable failures one deposit is allowed before the flush
    /// worker stops batching it. `flushBatch` is all-or-nothing and the oldest
    /// deposits are always batched first, so without a cap a single deposit
    /// that can never land blocks every newer one on its chain. `0` disables
    /// quarantine. Skipping is safe: the payer can still reclaim the deposit
    /// with `cancelDeposit`.
    #[serde(default = "default_flush_max_attempts")]
    pub flush_max_attempts: u32,
    /// Optional. When set, enables `withdrawNative` for this chain: the
    /// submitter targets this address, and the SNARK must name it as both
    /// `recipient` and `relayer`. Mirrors `NativeAdapter.sol` deployed
    /// alongside MASP, which is ERC-20 only.
    #[serde(default)]
    pub native_adapter_address: Option<String>,
    /// Optional. When set, enables `/v1/swap` for this chain. Submitter
    /// targets this address for swap calldata; spend ops still target
    /// `pool_address`. Mirrors `SwapWrapper.sol` deployed alongside MASP.
    #[serde(default)]
    pub swap_wrapper_address: Option<String>,
    /// Oracle base symbol for the chain's native gas token (e.g. "ETH",
    /// "BNB", "MATIC"). Used as `base` in Coinbase price lookups.
    #[serde(default = "default_native_symbol")]
    pub native_symbol: String,
    /// Native token decimals (18 for all EVM chains today).
    #[serde(default = "default_native_decimals")]
    pub native_decimals: u8,
    /// Per-chain markup applied on top of the raw gas cost. bps: 1000 = 10%.
    #[serde(default = "default_fee_markup_bps")]
    pub fee_markup_bps: u32,
    /// How long a swap the wallet did not pin a deadline on stays valid,
    /// in seconds. The wrapper reverts `SwapExpired` past it. Without a
    /// bound, a swap can sit in the mempool and execute at an arbitrarily
    /// later price with only `min_out` protecting the user.
    #[serde(default = "default_swap_deadline_s")]
    pub swap_default_deadline_s: u64,
    /// Accepted fee tokens for `/v1/spend/estimate` and `/v1/swap/estimate`.
    #[serde(default)]
    pub accepted_fee_tokens: Vec<FeeTokenCfg>,
    /// Wallet-facing description, served verbatim by `/chains`.
    #[serde(default)]
    pub public: ChainPublicCfg,
}

/// What a wallet needs in order to talk to this chain, and nothing the relayer
/// itself reads.
///
/// It lives here because the relayer is the only service that already
/// enumerates every chain, which makes it the natural registry to boot a
/// wallet from: a deployment can add a chain without rebuilding any frontend.
///
/// Every field is optional so existing configs keep booting. A client that
/// finds one absent falls back to its own build-time configuration, which is
/// exactly the single-chain behaviour that predates this.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ChainPublicCfg {
    /// Human label; also what `wallet_addEthereumChain` registers.
    #[serde(default)]
    pub name: Option<String>,
    /// Browser-reachable RPC.
    ///
    /// Deliberately separate from `ChainCfg::rpc_url`, which is the relayer's
    /// own endpoint and is typically cluster-internal — serving that one to a
    /// browser would hand out an unreachable URL.
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

#[derive(Debug, Deserialize, Clone)]
pub struct FeeTokenCfg {
    pub symbol: String,
    pub address: String,
    pub decimals: u8,
    /// Oracle quote symbol, e.g. "USDC".
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
    /// How much *further* past `cache_ttl_s` a cached price may be served when
    /// the upstream fetch fails. Measured from the end of the TTL, not from
    /// the fetch.
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

#[derive(Debug, Deserialize, Clone)]
pub struct ProverCfg {
    /// `circuits/build/tree_update_js/tree_update.wasm`.
    pub wasm_path: PathBuf,
    /// `circuits/build/tree_update.r1cs`.
    pub r1cs_path: PathBuf,
    /// `circuits/build/tree_update_final.zkey` (snarkjs-compatible).
    pub zkey_path: PathBuf,
    /// snarkjs `verification_key.json` for the deployed **transact** circuit
    /// (`circuits/build/3x3_verification_key.json`).
    ///
    /// Optional so a deployment that has not shipped the artifact still boots,
    /// but it should always be set: without it the relayer cannot tell a valid
    /// wallet proof from a fabricated one until the contract does, which means
    /// every junk payload costs a full `tree_update_batch` Groth16 first.
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
    8
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

/// `10u128.pow(n)` is the widest exponent that fits.
const MAX_DECIMALS: u8 = 38;
/// 100x. Anything above this is a typo, and `10_000 + bps` must not overflow.
const MAX_MARKUP_BPS: u32 = 1_000_000;

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
        out
    }
}

impl RelayerConfig {
    /// Overlay env vars on top of TOML defaults, per chain. Convention:
    ///   RELAYER_CHAIN_<id>_POOL_ADDRESS=0x…
    ///   RELAYER_CHAIN_<id>_RPC_URL=http://…
    ///   RELAYER_CHAIN_<id>_SIGNER_KEY=0x…
    /// Boot-time sanity checks on values the code later assumes are sane.
    ///
    /// Each of these is otherwise a runtime failure far from its cause: a
    /// duplicate chain silently runs two independent tree mirrors against one
    /// chain, and the decimal/markup bounds are arithmetic that panics rather
    /// than erroring.
    ///
    /// Every problem is reported, not just the first — an operator fixing a
    /// config should not have to restart once per mistake.
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
            public: ChainPublicCfg::default(),
        }
    }

    fn cfg(chains: Vec<ChainCfg>) -> RelayerConfig {
        RelayerConfig {
            database_url: "postgres://localhost/x".into(),
            listen_addr: "0.0.0.0:3003".into(),
            chains,
            prover: ProverCfg {
                wasm_path: "/w".into(),
                r1cs_path: "/r".into(),
                zkey_path: "/z".into(),
                transact_vkey_path: None,
            },
            price_oracle: PriceOracleCfg::default(),
        }
    }

    #[test]
    fn a_well_formed_config_validates() {
        cfg(vec![chain(1), chain(2)]).validate().unwrap();
    }

    /// A duplicate silently built two independent `TreeMirror`s and two flush
    /// workers for one chain — a guaranteed desync rather than a config typo.
    #[test]
    fn a_duplicate_chain_id_is_refused() {
        let err = cfg(vec![chain(1), chain(1)]).validate().unwrap_err();
        assert!(err.to_string().contains("more than once"), "got {err}");
    }

    /// An operator fixing a config should see every mistake at once.
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
}
