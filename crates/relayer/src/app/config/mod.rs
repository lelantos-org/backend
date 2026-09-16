//! `relayer.toml`: the config types and their defaults.
//!
//! - `validate`: boot-time checks, every problem reported at once.
//! - `env`: the `RELAYER_CHAIN_<id>_<FIELD>` overlay.

mod env;
mod validate;

pub use validate::ConfigErrors;

use crate::domain::batch::MAX_DEPOSITS_PER_BATCH;
use serde::Deserialize;
use std::path::PathBuf;

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
    /// Test-only routes; see [`TestHooksCfg`].
    #[serde(default)]
    pub test_hooks: TestHooksCfg,
}

/// Routes that let an end-to-end test hold a chain's batcher, inspect its queue
/// and release it, so a mixed bundle can be assembled deterministically.
///
/// Off unless set, and never set in a deployed config: holding the batcher stops
/// every submission on the chain. With `enabled = false` the routes are not
/// mounted at all.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct TestHooksCfg {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainCfg {
    pub chain_id: i64,
    pub rpc_url: String,
    /// MASP pool address. Read from and addressed in calldata; transactions go to
    /// [`Self::bundler_address`].
    pub pool_address: String,
    /// This relayer's `Bundler` clone, created with `BundlerFactory.create` from
    /// the relayer's owner key with `signer_key_hex`'s address as operator.
    ///
    /// Every submission is sent to it, so it is the pool's and the swap wrapper's
    /// caller, and it is the address `/chains` publishes for wallets to bind:
    /// `pi.relayer` on a pool spend and `pi_w.payer` on a swap. A native unshield
    /// still binds `native_adapter_address` as `pi.relayer`.
    pub bundler_address: String,
    /// Most operations one transaction carries. `1` sends every operation alone,
    /// as before bundling; see `pipeline::batcher` for how larger values are sized
    /// per chain.
    #[serde(default = "default_bundle_max_items")]
    pub bundle_max_items: usize,
    /// How long the batcher waits after taking the first queued operation for more
    /// to arrive. `0` sends as soon as it is free, which already bundles whatever
    /// queued behind the previous transaction.
    #[serde(default)]
    pub bundle_linger_ms: u64,
    /// Largest calldata a bundle may carry. A transaction over the node's or
    /// sequencer's size limit is refused at broadcast, which a gas estimate does
    /// not catch, so the batcher stops adding operations before reaching it.
    #[serde(default = "default_max_tx_bytes")]
    pub max_tx_bytes: usize,
    /// Relayer signer key, 32-byte hex. Sends every transaction, so its address
    /// must be an operator of `bundler_address`. Wallets bind the Bundler, not this
    /// key, so it can rotate without invalidating in-flight proofs.
    pub signer_key_hex: String,
    /// Account `/chains` advertises as `refundAddress`: where a wallet with no EVM
    /// account of its own may point a swap's `swap.refundTo`, so a cancelled
    /// output escrow lands somewhere able to move it. Absent means the address of
    /// `signer_key_hex`. Must be an account that can transfer tokens, never the
    /// Bundler or a swap wrapper, which swap validation refuses.
    #[serde(default)]
    pub refund_address: Option<String>,
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
    /// Seconds a batch smaller than `flush_max_n` waits for more deposits before
    /// it is flushed anyway. A flush's fixed cost is shared by its deposits, so a
    /// full batch is far cheaper per deposit. `0` flushes whatever is pending on
    /// every tick.
    #[serde(default)]
    pub flush_partial_after_s: u64,
    /// When set, enables `withdrawNative` for this chain: the Bundler calls this
    /// address and the SNARK must name it as both `recipient` and `relayer`.
    /// Mirrors `NativeAdapter.sol` deployed alongside MASP, which is ERC-20 only.
    #[serde(default)]
    pub native_adapter_address: Option<String>,
    /// When set, enables `/v1/swap` for this chain. The Bundler calls this address
    /// for swap calldata while spends call `pool_address`. Mirrors `SwapWrapper.sol`
    /// deployed alongside MASP.
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
    /// Applies to spends and deposits alike. A payer may pay the fee in a different
    /// asset than the one they are moving, so an asset left out of this list can
    /// still be moved; it just cannot pay for it. A deposit whose fee note is in
    /// an asset left out cannot be priced and is not flushed by this relayer.
    #[serde(default)]
    pub shielded_fee_assets: Vec<u64>,
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

/// Upper bound on `bundle_max_items`. Past it a bundle evicts most of the pool's
/// 64-root window by itself, and no chain's size or gas limit fits that many
/// operations anyway.
pub const MAX_BUNDLE_ITEMS: usize = 32;

/// One whole in basis points.
pub const BPS_DENOMINATOR: u32 = 10_000;

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
    // Derived, not restated: `app::state` already clamps to this figure, so a
    // literal here silently caps every flush at the old batch width the moment
    // `MAX_L_BATCH` moves — which is exactly what happened when it widened to 8
    // and this default stayed at the pre-widening 2.
    MAX_DEPOSITS_PER_BATCH
}

fn default_flush_max_attempts() -> u32 {
    5
}

fn default_bundle_max_items() -> usize {
    1
}

/// Below geth's and op-geth's 128 KB txpool limit with room for the envelope;
/// Arbitrum's sequencer takes 95 KB, so deployments there set it lower.
fn default_max_tx_bytes() -> usize {
    120_000
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

fn default_shielded_fee_grace_bps() -> u32 {
    300
}

#[cfg(test)]
mod tests;
