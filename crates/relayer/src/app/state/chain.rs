//! Booting one configured chain: its tree mirror, its batcher and the pipelines
//! that share it.

use crate::adapters::masp::MaspReader;
use crate::adapters::parse::{FieldRef, parse_field};
use crate::adapters::rpc::{RpcEndpoint, endpoint};
use crate::app::config::{ChainCfg, RelayerConfig};
use crate::domain::batch::MAX_DEPOSITS_PER_BATCH;
use crate::domain::error::{AppError, AppResult};
use crate::repositories::deposit_escrowed_events::DepositMempool;
use crate::services::events::EventBroadcaster;
use crate::services::fees::gas_estimator::GasEstimator;
use crate::services::fees::gas_witness::GasWitness;
use crate::services::fees::oracle::{CoinbaseOracle, PriceOracle};
use crate::services::fees::quote::{FeeQuoter, FeeToken};
use crate::services::fees::shielded::ShieldedFeeChecker;
use crate::services::pipeline::batcher::{self, Batcher, BatcherCfg, ResyncCtx};
use crate::services::pipeline::flush::failures::DepositFailures;
use crate::services::pipeline::{FlushPipeline, SpendPipeline, SwapPipeline, swap};
use crate::services::submitter::Submitter;
use crate::services::transact_verifier::TransactVerifier;
use crate::services::tree::TreeMirror;
use ::asset_registry::AssetRegistry;
use alloy::primitives::Address;
use crypto::tree::Field;
use database::DbPool;
use groth16::TreeUpdateBatchProver;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Dependencies every chain's pipelines share. Built once so the per-chain code
/// below states what each chain adds rather than repeating a list of clones.
pub(super) struct Shared {
    pub(super) pool: DbPool,
    pub(super) assets: Arc<AssetRegistry>,
    prover: Arc<dyn TreeUpdateBatchProver>,
    oracle: Arc<dyn PriceOracle>,
    pub(super) events: Arc<EventBroadcaster>,
    /// `None` when the deployment shipped no transact verification key.
    transact_verifier: Option<Arc<TransactVerifier>>,
}

impl Shared {
    pub(super) fn new(
        cfg: &RelayerConfig,
        pool: DbPool,
        prover: Arc<dyn TreeUpdateBatchProver>,
    ) -> AppResult<Self> {
        let oracle: Arc<dyn PriceOracle> = Arc::new(
            CoinbaseOracle::new(&cfg.price_oracle)
                .map_err(|e| AppError::Internal(format!("price oracle: {e}")))?,
        );
        Ok(Self {
            assets: Arc::new(AssetRegistry::new(pool.clone())),
            pool,
            prover,
            oracle,
            events: Arc::new(EventBroadcaster::new()),
            // The key describes the circuit rather than the deployment, so it is
            // loaded once rather than per chain.
            transact_verifier: load_transact_verifier(cfg)?,
        })
    }
}

fn load_transact_verifier(cfg: &RelayerConfig) -> AppResult<Option<Arc<TransactVerifier>>> {
    let Some(path) = &cfg.prover.transact_vkey_path else {
        warn!(
            "prover.transact_vkey_path is unset: wallet proofs are not checked before the \
             tree-update prove, so an invalid payload still costs a full Groth16"
        );
        return Ok(None);
    };
    let verifier = Arc::new(TransactVerifier::load(path)?);
    info!(vkey = %path.display(), "transact proof pre-verification enabled");
    Ok(Some(verifier))
}

/// What one configured chain contributes to the running relayer.
pub(super) struct ChainRuntime {
    pub(super) spend: Arc<SpendPipeline>,
    /// Present only where `swap_wrapper_address` is configured.
    pub(super) swap: Option<Arc<SwapPipeline>>,
    pub(super) flush: Arc<FlushPipeline>,
    pub(super) batcher: Batcher,
}

pub(super) async fn build_chain(c: &ChainCfg, shared: &Shared) -> AppResult<ChainRuntime> {
    let rpc = endpoint(&c.rpc_url).map_err(|e| boot_err(c.chain_id, "rpc endpoint", e))?;
    let pool_address = parse_configured_address(c.chain_id, "pool_address", &c.pool_address)?;
    let masp = MaspReader::new(&rpc, pool_address);
    let mirror = bootstrap_mirror(c, shared, &masp).await?;

    // Taken before the mirror goes behind its mutex, so `/chains` can read the
    // tree's state without waiting on a submission.
    let snapshot = mirror.snapshot();
    let mirror = Arc::new(Mutex::new(mirror));

    let bundler_address =
        parse_configured_address(c.chain_id, "bundler_address", &c.bundler_address)?;
    // The placeholder configs ship. Booting on it would send every bundle to the
    // zero address.
    if bundler_address.is_zero() {
        return Err(boot_err(
            c.chain_id,
            "bundler_address",
            "unset; create this relayer's Bundler with BundlerFactory.create",
        ));
    }
    let submitter = Arc::new(
        Submitter::new(
            c.chain_id,
            &rpc,
            &c.signer_key_hex,
            bundler_address,
            c.receipt_timeout_s,
            c.receipt_poll_interval_ms,
        )
        .map_err(|e| boot_err(c.chain_id, "bundler submitter", e))?,
    );
    // The operator's own account unless configured otherwise: an EOA that can
    // move a refund, unlike the Bundler wallets bind as submitter.
    let refund_address = match c.refund_address.as_deref() {
        Some(hex) => parse_configured_address(c.chain_id, "refund_address", hex)?,
        None => submitter.signer_address,
    };
    let native_adapter = parse_optional_address(
        c.chain_id,
        "native_adapter_address",
        c.native_adapter_address.as_deref(),
    )?;
    let wrapper_address = parse_optional_address(
        c.chain_id,
        "swap_wrapper_address",
        c.swap_wrapper_address.as_deref(),
    )?;
    // Swap validation refuses the same addresses, so advertising one would 400
    // every wallet that takes the offer.
    if let Some(why) = swap::refund_address_error(refund_address, wrapper_address, bundler_address)
    {
        return Err(boot_err(c.chain_id, "refund_address", why));
    }
    let fee_quoter = Arc::new(build_fee_quoter(c, shared, &rpc).await?);
    let gas_witness = Arc::new(GasWitness::new());
    let shielded_fee = build_shielded_fee_checker(c, shared, &fee_quoter)?;

    // Every tree-advancing operation goes out through this relayer's Bundler, so
    // one submitter, and so one nonce sequence, serves the whole chain.
    let batcher = batcher::spawn(BatcherCfg {
        chain_id: c.chain_id,
        mirror,
        prover: shared.prover.clone(),
        submitter,
        max_items: c.bundle_max_items,
        linger: Duration::from_millis(c.bundle_linger_ms),
        max_tx_bytes: c.max_tx_bytes,
        gas_witness: gas_witness.clone(),
        dry_run_verifiers: dry_run_verifiers(c, &rpc, &masp).await?,
        resync: ResyncCtx {
            pool: shared.pool.clone(),
            masp: masp.clone(),
        },
    });

    let spend = Arc::new(SpendPipeline {
        chain_id: c.chain_id,
        snapshot,
        batcher: batcher.clone(),
        pool_address,
        bundler_address,
        refund_address,
        native_adapter,
        fee_quoter: fee_quoter.clone(),
        gas_witness: gas_witness.clone(),
        transact_verifier: shared.transact_verifier.clone(),
        shielded_fee: shielded_fee.clone(),
        assets: shared.assets.clone(),
    });

    let swap = wrapper_address.map(|wrapper_address| {
        info!(chain_id = c.chain_id, wrapper = %wrapper_address, "swap pipeline ready");
        // Shares the chain's batcher with the spend and flush pipelines, so a
        // swap lands in the same bundles as their operations.
        Arc::new(SwapPipeline {
            chain_id: c.chain_id,
            batcher: batcher.clone(),
            wrapper_address,
            bundler_address,
            fee_quoter: fee_quoter.clone(),
            gas_witness: gas_witness.clone(),
            transact_verifier: shared.transact_verifier.clone(),
            shielded_fee: shielded_fee.clone(),
            assets: shared.assets.clone(),
        })
    });

    let flush = Arc::new(FlushPipeline {
        chain_id: c.chain_id,
        batcher: batcher.clone(),
        mempool: Arc::new(DepositMempool::new(shared.pool.clone(), c.chain_id)),
        masp,
        max_n: c.flush_max_n.clamp(1, MAX_DEPOSITS_PER_BATCH),
        partial_after: Duration::from_secs(c.flush_partial_after_s),
        partial_since: Default::default(),
        events: shared.events.clone(),
        failures: DepositFailures::new(c.chain_id, c.flush_max_attempts),
        shielded_fee: shielded_fee.clone(),
        gas_witness: gas_witness.clone(),
        fee_quoter: fee_quoter.clone(),
        assets: shared.assets.clone(),
    });

    info!(
        chain_id = c.chain_id,
        flush_interval_s = c.flush_interval_s,
        flush_max_n = flush.max_n,
        flush_max_attempts = c.flush_max_attempts,
        bundler = %bundler_address,
        bundle_max_items = c.bundle_max_items,
        swap = swap.is_some(),
        native = spend.native_adapter.is_some(),
        shielded_fee = shielded_fee.is_some(),
        "relayer pipelines ready"
    );
    Ok(ChainRuntime {
        spend,
        swap,
        flush,
        batcher,
    })
}

/// The pool's two verifier addresses, which a dry run stubs out, or `None` when
/// the chain's RPC ignores code overrides and dry runs are skipped.
///
/// Without dry runs a failing operation is still caught by the simulation of the
/// proved bundle, just after its successors' proofs were paid for.
async fn dry_run_verifiers(
    c: &ChainCfg,
    rpc: &RpcEndpoint,
    masp: &MaspReader,
) -> AppResult<Option<[Address; 2]>> {
    if !batcher::supports_code_overrides(rpc).await {
        warn!(
            chain_id = c.chain_id,
            "rpc ignores eth_call code overrides; bundles are not dry-run before proving"
        );
        return Ok(None);
    }
    let verifiers = masp
        .verifiers()
        .await
        .map_err(|e| boot_err(c.chain_id, "dry-run verifiers", e))?;
    Ok(Some(verifiers))
}

/// Replay the chain's tree from the indexer's tables, then check the result
/// against the pool itself. Both must agree before this chain serves anything.
async fn bootstrap_mirror(
    c: &ChainCfg,
    shared: &Shared,
    masp: &MaspReader,
) -> AppResult<TreeMirror> {
    let mut mirror =
        TreeMirror::new(c.chain_id).map_err(|e| boot_err(c.chain_id, "mirror init", e))?;
    mirror
        .bootstrap(&shared.pool)
        .await
        .map_err(|e| boot_err(c.chain_id, "bootstrap", e))?;
    mirror
        .verify_chain_root(masp)
        .await
        .map_err(|e| boot_err(c.chain_id, "chain root check", e))?;
    Ok(mirror)
}

/// Optional shielded fee collection.
///
/// Refuses to boot on any combination that would appear configured and behave
/// otherwise: a key that does not match its address, checked inside
/// [`ShieldedFeeChecker::new`], a fee table that can price nothing, or a missing
/// transact verification key.
fn build_shielded_fee_checker(
    c: &ChainCfg,
    shared: &Shared,
    fee_quoter: &Arc<FeeQuoter>,
) -> AppResult<Option<Arc<ShieldedFeeChecker>>> {
    let Some(settings) = c.shielded_fee() else {
        return Ok(None);
    };
    let fail = |why: &str| boot_err(c.chain_id, "shielded fee", why);

    // Without a transact verification key, `out_cm` and `nullifier[0]` reach the
    // fee check unverified, and those are what bind a decrypted value to the
    // proof. The fee would then rest on the caller's assertion, so boot fails
    // instead.
    if shared.transact_verifier.is_none() {
        return Err(fail(
            "shielded fees require prover.transact_vkey_path: without it a wallet's proof is \
             not checked before submission, so the public inputs a fee is bound to are \
             unverified",
        ));
    }
    // An asset with no price cannot be quoted, so a fee in it would be refused at
    // submit time and appear to the payer as a broken relayer. Whether each
    // individual asset is priced cannot be settled here, since the asset-id to
    // token-address mapping lives in a table the indexer may not have filled, but
    // an empty fee table settles all of them at once.
    if c.accepted_fee_tokens.is_empty() {
        return Err(fail(
            "no accepted_fee_tokens are configured, so no fee can be priced and every spend \
             would be refused",
        ));
    }

    let ivk = parse_configured_field(c.chain_id, "shielded_fee_ivk", settings.ivk)?;
    let checker = ShieldedFeeChecker::new(
        c.chain_id,
        settings,
        ivk,
        fee_quoter.clone(),
        shared.assets.clone(),
    )?;

    info!(
        chain_id = c.chain_id,
        grace_bps = settings.grace_bps,
        assets = settings.assets.len(),
        "shielded fee collection enabled"
    );
    Ok(Some(Arc::new(checker)))
}

async fn build_fee_quoter(
    c: &ChainCfg,
    shared: &Shared,
    rpc: &RpcEndpoint,
) -> AppResult<FeeQuoter> {
    let accepted_fee_tokens: Vec<FeeToken> = c
        .accepted_fee_tokens
        .iter()
        .map(FeeToken::from_cfg)
        .collect::<AppResult<_>>()?;
    validate_fee_token_pairs(
        shared.oracle.as_ref(),
        &c.native_symbol,
        &accepted_fee_tokens,
        c.chain_id,
    )
    .await?;
    Ok(FeeQuoter {
        chain_id: c.chain_id,
        native_symbol: c.native_symbol.clone(),
        native_decimals: c.native_decimals,
        accepted_fee_tokens,
        oracle: shared.oracle.clone(),
        gas_estimator: Arc::new(GasEstimator::new(c.chain_id, rpc.clone())),
        markup_bps: c.fee_markup_bps,
    })
}

/// Boot-time check that every accepted fee token resolves a price through the
/// configured oracle, so a misconfigured `quote_symbol` fails at startup rather
/// than at the first `/estimate` call.
async fn validate_fee_token_pairs(
    oracle: &dyn PriceOracle,
    native_symbol: &str,
    fee_tokens: &[FeeToken],
    chain_id: i64,
) -> AppResult<()> {
    for t in fee_tokens {
        oracle
            .price(native_symbol, &t.quote_symbol)
            .await
            .map_err(|e| {
                boot_err(
                    chain_id,
                    "fee token validation",
                    format!(
                        "pair {}-{} not resolvable: {}",
                        native_symbol, t.quote_symbol, e
                    ),
                )
            })?;
    }
    info!(
        chain_id,
        native = native_symbol,
        tokens = fee_tokens.len(),
        "fee token oracle pairs validated"
    );
    Ok(())
}

/// A 32-byte field element from config, in this crate's big-endian convention.
fn parse_configured_field(chain_id: i64, field: &'static str, value: &str) -> AppResult<Field> {
    parse_field(value, FieldRef::Named(field))
        .map(|b| b.0)
        .map_err(|e| boot_err(chain_id, field, e))
}

fn parse_configured_address(chain_id: i64, field: &str, hex: &str) -> AppResult<Address> {
    Address::from_str(hex).map_err(|e| boot_err(chain_id, field, e))
}

fn parse_optional_address(
    chain_id: i64,
    field: &str,
    hex: Option<&str>,
) -> AppResult<Option<Address>> {
    hex.map(|hex| parse_configured_address(chain_id, field, hex))
        .transpose()
}

/// Boot failures are all fatal and share the same chain-and-step framing, so they
/// share one constructor.
fn boot_err(chain_id: i64, step: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("chain {chain_id}: {step}: {e}"))
}
