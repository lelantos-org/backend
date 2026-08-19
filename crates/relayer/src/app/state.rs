use crate::adapters::calldata::MAX_L_BATCH;
use crate::adapters::rpc::RpcEndpoint;
use crate::app::config::{ChainCfg, ChainPublicCfg, RelayerConfig};
use crate::domain::error::AppError;
use crate::domain::error::AppResult;
use crate::domain::responses::ChainConfigOut;
use crate::services::deposit_mempool::DepositMempool;
use crate::services::events::EventBroadcaster;
use crate::services::fee_quote::{FeeQuoter, FeeToken};
use crate::services::gas_estimator::GasEstimator;
use crate::services::gas_witness::GasWitness;
use crate::services::idempotency::IdempotencyCache;
use crate::services::nullifier_guard::NullifierGuards;
use crate::services::oracle::{CoinbaseOracle, PriceOracle};
use crate::services::pipeline::{FlushPipeline, NativeRoute, SpendPipeline, SwapPipeline};
use crate::services::prover::TreeUpdateBatchProver;
use crate::services::submitter::Submitter;
use crate::services::transact_verifier::TransactVerifier;
use crate::services::tree::{self, TreeMirror};
use alloy::primitives::Address;
use database::DbPool;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

#[derive(Clone)]
pub struct AppState {
    /// One spend pipeline per chain. HTTP `/v1/spend` looks up by `payload.chain_id`.
    pub spend_pipelines: Arc<HashMap<i64, Arc<SpendPipeline>>>,
    /// Built only for chains where `swap_wrapper_address` is configured.
    /// HTTP `/v1/swap` looks up by `payload.chain_id`.
    pub swap_pipelines: Arc<HashMap<i64, Arc<SwapPipeline>>>,
    /// Process-wide deposit lifecycle pub/sub. SSE handler subscribes;
    /// `FlushPipeline` publishes after each successful `flushBatch`.
    pub events: Arc<EventBroadcaster>,
    /// DB pool. Used by `nullifier_guard` for `spent_nullifiers` lookups
    /// before SNARK generation.
    pub pool: DbPool,
    /// Nullifier admission control. See `services::nullifier_guard`.
    pub nullifiers: Arc<NullifierGuards>,
    /// Replays a submission a caller has already made under the same
    /// `Idempotency-Key`. See `services::idempotency`.
    pub idempotency: Arc<IdempotencyCache>,
    /// Wallet-facing description of each chain, resolved once at boot and
    /// served by `/chains`. Holds only what a client may see — never the
    /// signer key or the relayer's internal RPC.
    pub descriptors: Arc<HashMap<i64, ChainDescriptor>>,
}

/// The half of `ChainCfg` that is safe to publish.
///
/// Built at boot rather than read per request so the handler cannot reach the
/// rest of the config, and so a malformed address fails startup instead of a
/// wallet's first call.
pub struct ChainDescriptor {
    pub native_adapter_address: Option<String>,
    pub swap_wrapper_address: Option<String>,
    pub public: ChainPublicCfg,
}

impl ChainDescriptor {
    fn from_cfg(c: &ChainCfg) -> Self {
        Self {
            native_adapter_address: c.native_adapter_address.clone(),
            swap_wrapper_address: c.swap_wrapper_address.clone(),
            public: c.public.clone(),
        }
    }
}

/// Owned by the descriptor rather than assembled in the handler: it is the
/// only place that knows which parts of a `ChainCfg` may be published, so
/// adding a field to the config cannot leak into `/chains` by accident.
impl From<&ChainDescriptor> for ChainConfigOut {
    fn from(d: &ChainDescriptor) -> Self {
        Self {
            native_adapter_address: d.native_adapter_address.clone(),
            swap_wrapper_address: d.swap_wrapper_address.clone(),
            chain_name: d.public.name.clone(),
            rpc_url: d.public.rpc_url.clone(),
            tree_depth: d.public.tree_depth,
            permit2_address: d.public.permit2_address.clone(),
            explorer_url: d.public.explorer_url.clone(),
        }
    }
}

impl AppState {
    /// The spend pipeline serving `chain_id`, or a 404.
    ///
    /// Every endpoint dispatches on a chain id the caller supplied, so the
    /// "unknown chain" answer belongs here rather than being restated at each
    /// one.
    pub fn spend_pipeline(&self, chain_id: i64) -> AppResult<Arc<SpendPipeline>> {
        self.spend_pipelines
            .get(&chain_id)
            .cloned()
            .ok_or(AppError::UnknownChain(chain_id))
    }

    /// The swap pipeline serving `chain_id`. Absent on chains with no
    /// `swap_wrapper_address`, which is the same 404 to a caller.
    pub fn swap_pipeline(&self, chain_id: i64) -> AppResult<Arc<SwapPipeline>> {
        self.swap_pipelines
            .get(&chain_id)
            .cloned()
            .ok_or(AppError::UnknownChain(chain_id))
    }

    pub fn serves_chain(&self, chain_id: i64) -> bool {
        self.spend_pipelines.contains_key(&chain_id)
    }
}

pub async fn build_state(
    cfg: &RelayerConfig,
    pool: DbPool,
    prover: Arc<dyn TreeUpdateBatchProver>,
) -> AppResult<AppState> {
    let shared = Shared::new(cfg, pool.clone(), prover)?;

    let mut spend_pipelines: HashMap<i64, Arc<SpendPipeline>> = HashMap::new();
    let mut swap_pipelines: HashMap<i64, Arc<SwapPipeline>> = HashMap::new();
    for c in &cfg.chains {
        let chain = build_chain(c, &shared).await?;
        spawn_flush_worker(chain.flush, Duration::from_secs(c.flush_interval_s));
        spend_pipelines.insert(c.chain_id, chain.spend);
        if let Some(swap) = chain.swap {
            swap_pipelines.insert(c.chain_id, swap);
        }
    }

    Ok(AppState {
        spend_pipelines: Arc::new(spend_pipelines),
        swap_pipelines: Arc::new(swap_pipelines),
        events: shared.events,
        pool,
        nullifiers: Arc::new(NullifierGuards::new(cfg.chains.iter().map(|c| c.chain_id))),
        idempotency: Arc::new(IdempotencyCache::new()),
        descriptors: Arc::new(
            cfg.chains
                .iter()
                .map(|c| (c.chain_id, ChainDescriptor::from_cfg(c)))
                .collect(),
        ),
    })
}

/// Dependencies every chain's pipelines share. Built once so the per-chain
/// code below reads as "what this chain adds", not as a list of clones.
struct Shared {
    pool: DbPool,
    prover: Arc<dyn TreeUpdateBatchProver>,
    oracle: Arc<dyn PriceOracle>,
    events: Arc<EventBroadcaster>,
    /// `None` when the deployment shipped no transact verification key.
    transact_verifier: Option<Arc<TransactVerifier>>,
}

impl Shared {
    fn new(
        cfg: &RelayerConfig,
        pool: DbPool,
        prover: Arc<dyn TreeUpdateBatchProver>,
    ) -> AppResult<Self> {
        let oracle: Arc<dyn PriceOracle> = Arc::new(
            CoinbaseOracle::new(&cfg.price_oracle)
                .map_err(|e| AppError::Internal(format!("price oracle: {e}")))?,
        );
        Ok(Self {
            pool,
            prover,
            oracle,
            events: Arc::new(EventBroadcaster::new()),
            // The key describes the circuit, not the deployment, so it is
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
struct ChainRuntime {
    spend: Arc<SpendPipeline>,
    /// Present only where `swap_wrapper_address` is configured.
    swap: Option<Arc<SwapPipeline>>,
    flush: Arc<FlushPipeline>,
}

async fn build_chain(c: &ChainCfg, shared: &Shared) -> AppResult<ChainRuntime> {
    let rpc = RpcEndpoint::new(&c.rpc_url).map_err(|e| boot_err(c.chain_id, "rpc endpoint", e))?;
    let mirror = bootstrap_mirror(c, shared, &rpc).await?;

    // Taken before the mirror goes behind its mutex, so `/chains` can read the
    // tree's state without waiting on a submission.
    let snapshot = mirror.snapshot();
    let mirror = Arc::new(Mutex::new(mirror));

    let submitter = submitter_for(c, &rpc, &c.pool_address, "submitter")?;
    let fee_quoter = Arc::new(build_fee_quoter(c, shared, &rpc).await?);
    let gas_witness = Arc::new(GasWitness::new());

    let spend = Arc::new(SpendPipeline {
        chain_id: c.chain_id,
        mirror: mirror.clone(),
        snapshot,
        submitter: submitter.clone(),
        prover: shared.prover.clone(),
        fee_quoter: fee_quoter.clone(),
        gas_witness: gas_witness.clone(),
        native: build_native_route(c, &rpc)?,
        transact_verifier: shared.transact_verifier.clone(),
    });

    let swap = build_swap_pipeline(c, shared, &rpc, &mirror, &fee_quoter, &gas_witness)?;

    let flush = Arc::new(FlushPipeline {
        chain_id: c.chain_id,
        mirror,
        submitter,
        prover: shared.prover.clone(),
        mempool: Arc::new(DepositMempool::new(shared.pool.clone(), c.chain_id)),
        max_n: c.flush_max_n.clamp(1, MAX_L_BATCH),
        events: shared.events.clone(),
    });

    info!(
        chain_id = c.chain_id,
        flush_interval_s = c.flush_interval_s,
        flush_max_n = flush.max_n,
        swap = swap.is_some(),
        native = spend.native.is_some(),
        "relayer pipelines ready"
    );
    Ok(ChainRuntime { spend, swap, flush })
}

/// Replay the chain's tree from the indexer's tables, then prove the result
/// against the pool itself. Both must agree before this chain serves anything.
async fn bootstrap_mirror(
    c: &ChainCfg,
    shared: &Shared,
    rpc: &RpcEndpoint,
) -> AppResult<TreeMirror> {
    if let Some(declared) = c.public.tree_depth
        && declared as usize != tree::DEPTH
    {
        return Err(boot_err(
            c.chain_id,
            "tree depth",
            format!(
                "public.tree_depth is {declared} but this relayer mirrors a depth-{} tree; \
                 wallets would build proofs against the wrong shape",
                tree::DEPTH
            ),
        ));
    }
    let mut mirror =
        TreeMirror::new(c.chain_id).map_err(|e| boot_err(c.chain_id, "mirror init", e))?;
    mirror
        .bootstrap(&shared.pool)
        .await
        .map_err(|e| boot_err(c.chain_id, "bootstrap", e))?;
    mirror
        .verify_chain_root(rpc, &c.pool_address)
        .await
        .map_err(|e| boot_err(c.chain_id, "chain root check", e))?;
    Ok(mirror)
}

/// Optional native route. The adapter is the pool's caller for a native
/// unshield, so it needs its own submitter target; the tree mirror and prover
/// stay shared with every other entry point.
fn build_native_route(c: &ChainCfg, rpc: &RpcEndpoint) -> AppResult<Option<Arc<NativeRoute>>> {
    let Some(hex) = &c.native_adapter_address else {
        return Ok(None);
    };
    let address = parse_configured_address(c.chain_id, "native_adapter_address", hex)?;
    let submitter = submitter_for(c, rpc, hex, "native submitter")?;
    info!(chain_id = c.chain_id, adapter = %address, "native adapter route ready");
    Ok(Some(Arc::new(NativeRoute { address, submitter })))
}

/// Optional swap pipeline. Shares the chain's `TreeMirror` and prover with the
/// spend pipeline so the per-chain mutex serialises across both; its dedicated
/// submitter targets the wrapper rather than the pool.
fn build_swap_pipeline(
    c: &ChainCfg,
    shared: &Shared,
    rpc: &RpcEndpoint,
    mirror: &Arc<Mutex<TreeMirror>>,
    fee_quoter: &Arc<FeeQuoter>,
    gas_witness: &Arc<GasWitness>,
) -> AppResult<Option<Arc<SwapPipeline>>> {
    let Some(hex) = &c.swap_wrapper_address else {
        return Ok(None);
    };
    let wrapper_address = parse_configured_address(c.chain_id, "swap_wrapper_address", hex)?;
    let pipeline = SwapPipeline {
        chain_id: c.chain_id,
        mirror: mirror.clone(),
        submitter: submitter_for(c, rpc, hex, "swap submitter")?,
        prover: shared.prover.clone(),
        wrapper_address,
        fee_quoter: fee_quoter.clone(),
        gas_witness: gas_witness.clone(),
        default_deadline_s: c.swap_default_deadline_s,
        transact_verifier: shared.transact_verifier.clone(),
    };
    info!(chain_id = c.chain_id, wrapper = %wrapper_address, "swap pipeline ready");
    Ok(Some(Arc::new(pipeline)))
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

/// Every submitter on a chain shares its signer and receipt settings and
/// differs only in the contract it targets.
fn submitter_for(
    c: &ChainCfg,
    rpc: &RpcEndpoint,
    target_hex: &str,
    what: &str,
) -> AppResult<Arc<Submitter>> {
    Submitter::new(
        c.chain_id,
        rpc.clone(),
        &c.signer_key_hex,
        target_hex,
        c.receipt_timeout_s,
        c.receipt_poll_interval_ms,
    )
    .map(Arc::new)
    .map_err(|e| boot_err(c.chain_id, what, e))
}

fn parse_configured_address(chain_id: i64, field: &str, hex: &str) -> AppResult<Address> {
    Address::from_str(hex).map_err(|e| boot_err(chain_id, field, e))
}

/// Boot failures are all fatal and all want the same "which chain, which step"
/// framing, so they share one constructor.
fn boot_err(chain_id: i64, step: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Internal(format!("chain {chain_id}: {step}: {e}"))
}

/// Drive one chain's flush pipeline on a fixed interval.
///
/// Ticks are skipped rather than queued when one runs long, and a parked mirror
/// stops the worker outright: it never un-parks without a restart, so retrying
/// would only spam the log.
fn spawn_flush_worker(flush: Arc<FlushPipeline>, interval: Duration) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match flush.tick().await {
                Ok(_) => {}
                Err(e @ AppError::MirrorDesynced(_)) => {
                    error!(chain_id = flush.chain_id, error = %e, "flush worker stopping");
                    return;
                }
                // The prover was busy with another chain's proof. Expected
                // under load, and the next tick is seconds away.
                Err(AppError::ProverBusy) => {
                    debug!(chain_id = flush.chain_id, "flush deferred; prover busy")
                }
                Err(e) => warn!(chain_id = flush.chain_id, error = %e, "flush tick failed"),
            }
        }
    });
}

/// Boot-time check: every accepted fee token must resolve a price via the
/// configured oracle. Fail-fast prevents discovering a misconfigured
/// `quote_symbol` at first `/estimate` call.
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

#[cfg(test)]
mod tests {
    use super::{ChainConfigOut, ChainDescriptor, ChainPublicCfg};

    /// The conversion hand-copies seven fields, which is exactly the shape of
    /// change where one gets dropped and a wallet silently falls back to its
    /// own default instead of the deployment's value.
    #[test]
    fn test_chain_config_out_carries_every_described_field() {
        let d = ChainDescriptor {
            native_adapter_address: Some("0xNATIVE".to_string()),
            swap_wrapper_address: Some("0xSWAP".to_string()),
            public: ChainPublicCfg {
                name: Some("anvil".to_string()),
                rpc_url: Some("http://localhost:8545".to_string()),
                tree_depth: Some(10),
                permit2_address: Some("0xPERMIT2".to_string()),
                explorer_url: Some("http://explorer".to_string()),
            },
        };

        let out = ChainConfigOut::from(&d);

        assert_eq!(out.native_adapter_address.as_deref(), Some("0xNATIVE"));
        assert_eq!(out.swap_wrapper_address.as_deref(), Some("0xSWAP"));
        assert_eq!(out.chain_name.as_deref(), Some("anvil"));
        assert_eq!(out.rpc_url.as_deref(), Some("http://localhost:8545"));
        assert_eq!(out.tree_depth, Some(10));
        assert_eq!(out.permit2_address.as_deref(), Some("0xPERMIT2"));
        assert_eq!(out.explorer_url.as_deref(), Some("http://explorer"));
    }

    /// An operator who has not filled the block in yields an empty record, not
    /// a partly-invented one.
    #[test]
    fn test_chain_config_out_is_empty_when_nothing_is_described() {
        let out = ChainConfigOut::from(&ChainDescriptor {
            native_adapter_address: None,
            swap_wrapper_address: None,
            public: ChainPublicCfg::default(),
        });

        assert_eq!(out.chain_name, None);
        assert_eq!(out.rpc_url, None);
        assert_eq!(out.tree_depth, None);
    }
}
