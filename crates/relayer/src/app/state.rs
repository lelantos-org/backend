use crate::adapters::calldata::MAX_L_BATCH;
use crate::adapters::rpc::RpcEndpoint;
use crate::app::config::RelayerConfig;
use crate::domain::error::AppError;
use crate::domain::error::AppResult;
use crate::services::deposit_mempool::DepositMempool;
use crate::services::events::EventBroadcaster;
use crate::services::fee_quote::{FeeQuoter, FeeToken};
use crate::services::gas_estimator::GasEstimator;
use crate::services::gas_witness::GasWitness;
use crate::services::nullifier_guard::NullifierGuards;
use crate::services::oracle::{CoinbaseOracle, PriceOracle};
use crate::services::pipeline::{FlushPipeline, NativeRoute, SpendPipeline, SwapPipeline};
use crate::services::prover::TreeUpdateBatchProver;
use crate::services::submitter::Submitter;
use crate::services::tree::TreeMirror;
use alloy::primitives::Address;
use database::DbPool;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

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
}

pub async fn build_state(
    cfg: &RelayerConfig,
    pool: DbPool,
    prover: Arc<dyn TreeUpdateBatchProver>,
) -> AppResult<AppState> {
    let events = Arc::new(EventBroadcaster::new());
    let oracle: Arc<dyn PriceOracle> = Arc::new(
        CoinbaseOracle::new(&cfg.price_oracle)
            .map_err(|e| AppError::Internal(format!("price oracle: {e}")))?,
    );
    let mut spend_pipelines: HashMap<i64, Arc<SpendPipeline>> = HashMap::new();
    let mut swap_pipelines: HashMap<i64, Arc<SwapPipeline>> = HashMap::new();
    let nullifiers = Arc::new(NullifierGuards::new(cfg.chains.iter().map(|c| c.chain_id)));
    for c in &cfg.chains {
        let rpc = RpcEndpoint::new(&c.rpc_url)
            .map_err(|e| AppError::Internal(format!("rpc endpoint chain {}: {}", c.chain_id, e)))?;
        let mut mirror = TreeMirror::new(c.chain_id)
            .map_err(|e| AppError::Internal(format!("mirror init chain {}: {}", c.chain_id, e)))?;
        mirror
            .bootstrap(&pool)
            .await
            .map_err(|e| AppError::Internal(format!("bootstrap chain {}: {}", c.chain_id, e)))?;
        mirror
            .verify_chain_root(&rpc, &c.pool_address)
            .await
            .map_err(|e| AppError::Internal(format!("chain root check {}: {}", c.chain_id, e)))?;

        let mirror = Arc::new(Mutex::new(mirror));
        let submitter = Arc::new(
            Submitter::new(
                c.chain_id,
                rpc.clone(),
                &c.signer_key_hex,
                &c.pool_address,
                c.receipt_timeout_s,
                c.receipt_poll_interval_ms,
            )
            .map_err(|e| {
                AppError::Internal(format!("submitter init chain {}: {}", c.chain_id, e))
            })?,
        );

        let fee_tokens: Vec<FeeToken> = c
            .accepted_fee_tokens
            .iter()
            .map(FeeToken::from_cfg)
            .collect::<AppResult<_>>()?;
        validate_fee_token_pairs(oracle.as_ref(), &c.native_symbol, &fee_tokens, c.chain_id)
            .await?;
        let gas_estimator = Arc::new(GasEstimator::new(c.chain_id, rpc.clone()));
        let gas_witness = Arc::new(GasWitness::new());
        let fee_quoter = Arc::new(FeeQuoter {
            chain_id: c.chain_id,
            native_symbol: c.native_symbol.clone(),
            native_decimals: c.native_decimals,
            accepted_fee_tokens: fee_tokens,
            oracle: oracle.clone(),
            gas_estimator,
            markup_bps: c.fee_markup_bps,
        });

        // Optional native route. The adapter is the pool's caller for a
        // native unshield, so it needs its own submitter target; the tree
        // mirror and prover stay shared with every other entry point.
        let native = match &c.native_adapter_address {
            Some(hex) => {
                let address = Address::from_str(hex).map_err(|e| {
                    AppError::Internal(format!(
                        "native_adapter_address chain {}: {}",
                        c.chain_id, e
                    ))
                })?;
                let native_submitter = Arc::new(
                    Submitter::new(
                        c.chain_id,
                        rpc.clone(),
                        &c.signer_key_hex,
                        hex,
                        c.receipt_timeout_s,
                        c.receipt_poll_interval_ms,
                    )
                    .map_err(|e| {
                        AppError::Internal(format!(
                            "native submitter init chain {}: {}",
                            c.chain_id, e
                        ))
                    })?,
                );
                info!(chain_id = c.chain_id, adapter = %address, "native adapter route ready");
                Some(Arc::new(NativeRoute {
                    address,
                    submitter: native_submitter,
                }))
            }
            None => None,
        };

        let spend = SpendPipeline {
            chain_id: c.chain_id,
            mirror: mirror.clone(),
            submitter: submitter.clone(),
            prover: prover.clone(),
            fee_quoter: fee_quoter.clone(),
            gas_witness: gas_witness.clone(),
            native,
        };
        spend_pipelines.insert(c.chain_id, Arc::new(spend));

        // Optional swap pipeline. Same TreeMirror + prover as the spend
        // pipeline so the per-chain mutex serialises across both. Dedicated
        // submitter targets the wrapper address rather than MASP.
        if let Some(wrapper_hex) = &c.swap_wrapper_address {
            let wrapper_address = Address::from_str(wrapper_hex).map_err(|e| {
                AppError::Internal(format!("swap_wrapper_address chain {}: {}", c.chain_id, e))
            })?;
            let swap_submitter = Arc::new(
                Submitter::new(
                    c.chain_id,
                    rpc.clone(),
                    &c.signer_key_hex,
                    wrapper_hex,
                    c.receipt_timeout_s,
                    c.receipt_poll_interval_ms,
                )
                .map_err(|e| {
                    AppError::Internal(format!("swap submitter init chain {}: {}", c.chain_id, e))
                })?,
            );
            let swap = SwapPipeline {
                chain_id: c.chain_id,
                mirror: mirror.clone(),
                submitter: swap_submitter,
                prover: prover.clone(),
                wrapper_address,
                fee_quoter: fee_quoter.clone(),
                gas_witness: gas_witness.clone(),
            };
            swap_pipelines.insert(c.chain_id, Arc::new(swap));
            info!(
                chain_id = c.chain_id,
                wrapper = %wrapper_address,
                "swap pipeline ready"
            );
        }

        let max_n = c.flush_max_n.clamp(1, MAX_L_BATCH);
        let mempool = Arc::new(DepositMempool::new(pool.clone(), c.chain_id));
        let flush = Arc::new(FlushPipeline {
            chain_id: c.chain_id,
            mirror,
            submitter,
            prover: prover.clone(),
            mempool,
            max_n,
            events: events.clone(),
        });
        let interval = std::time::Duration::from_secs(c.flush_interval_s);
        let flush_task = flush.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                match flush_task.tick().await {
                    Ok(Some(_)) | Ok(None) => {}
                    // A parked mirror never un-parks without a restart, so
                    // keep the loop from retrying (and log-spamming) forever.
                    Err(e @ AppError::MirrorDesynced(_)) => {
                        error!(chain_id = flush_task.chain_id, error = %e, "flush worker stopping");
                        return;
                    }
                    Err(e) => {
                        warn!(chain_id = flush_task.chain_id, error = %e, "flush tick failed")
                    }
                }
            }
        });

        info!(
            chain_id = c.chain_id,
            flush_interval_s = c.flush_interval_s,
            flush_max_n = max_n,
            "relayer pipelines ready"
        );
    }

    Ok(AppState {
        spend_pipelines: Arc::new(spend_pipelines),
        swap_pipelines: Arc::new(swap_pipelines),
        events,
        pool,
        nullifiers,
    })
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
                AppError::Internal(format!(
                    "fee token validation chain {}: pair {}-{} not resolvable: {}",
                    chain_id, native_symbol, t.quote_symbol, e
                ))
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
