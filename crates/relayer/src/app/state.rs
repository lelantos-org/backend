use crate::adapters::calldata::MAX_N_BATCH;
use crate::app::config::RelayerConfig;
use crate::domain::error::AppError;
use crate::domain::error::AppResult;
use crate::services::events::EventBroadcaster;
use crate::services::fee_quote::{FeeQuoter, FeeToken};
use crate::services::gas_estimator::GasEstimator;
use crate::services::intent_mempool::IntentMempool;
use crate::services::nullifier_guard::PendingMap;
use crate::services::oracle::{CoinbaseOracle, PriceOracle};
use crate::services::pipeline::{FlushPipeline, SpendPipeline, SwapPipeline};
use crate::services::prover::TreeUpdateBatchProver;
use crate::services::submitter::Submitter;
use crate::services::tree::TreeMirror;
use alloy::primitives::Address;
use database::DbPool;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

#[derive(Clone)]
pub struct AppState {
    /// One spend pipeline per chain. HTTP `/v1/spend` looks up by `payload.chain_id`.
    pub spend_pipelines: Arc<HashMap<i64, Arc<SpendPipeline>>>,
    /// Built only for chains where `swap_wrapper_address` is configured.
    /// HTTP `/v1/swap` looks up by `payload.chain_id`.
    pub swap_pipelines: Arc<HashMap<i64, Arc<SwapPipeline>>>,
    /// Process-wide intent lifecycle pub/sub. SSE handler subscribes;
    /// `FlushPipeline` publishes after each successful `flushBatch`.
    pub events: Arc<EventBroadcaster>,
    /// DB pool. Used by `nullifier_guard` for `spent_nullifiers` lookups
    /// before SNARK generation.
    pub pool: DbPool,
    /// Per-chain set of nullifiers currently in flight through a spend or
    /// swap pipeline. See `services::nullifier_guard`.
    pub pending_nullifiers: PendingMap,
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
    let mut pending_nullifiers: HashMap<i64, Arc<Mutex<HashSet<[u8; 32]>>>> = HashMap::new();
    for c in &cfg.chains {
        pending_nullifiers.insert(c.chain_id, Arc::new(Mutex::new(HashSet::new())));
        let mut mirror = TreeMirror::new(c.chain_id)
            .map_err(|e| AppError::Internal(format!("mirror init chain {}: {}", c.chain_id, e)))?;
        mirror
            .bootstrap(&pool)
            .await
            .map_err(|e| AppError::Internal(format!("bootstrap chain {}: {}", c.chain_id, e)))?;
        mirror
            .verify_chain_root(&c.rpc_url, &c.pool_address)
            .await
            .map_err(|e| AppError::Internal(format!("chain root check {}: {}", c.chain_id, e)))?;

        let mirror = Arc::new(Mutex::new(mirror));
        let submitter = Arc::new(
            Submitter::new(
                c.chain_id,
                &c.rpc_url,
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
        let gas_estimator = Arc::new(GasEstimator::new(
            c.chain_id,
            &c.rpc_url,
            submitter.signer_address,
        ));
        let fee_quoter = Arc::new(FeeQuoter {
            chain_id: c.chain_id,
            native_symbol: c.native_symbol.clone(),
            native_decimals: c.native_decimals,
            accepted_fee_tokens: fee_tokens,
            oracle: oracle.clone(),
            gas_estimator,
            markup_bps: c.fee_markup_bps,
        });

        let spend = SpendPipeline {
            chain_id: c.chain_id,
            mirror: mirror.clone(),
            submitter: submitter.clone(),
            prover: prover.clone(),
            fee_quoter: fee_quoter.clone(),
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
                    &c.rpc_url,
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
            };
            swap_pipelines.insert(c.chain_id, Arc::new(swap));
            info!(
                chain_id = c.chain_id,
                wrapper = %wrapper_address,
                "swap pipeline ready"
            );
        }

        let max_n = c.flush_max_n.clamp(1, MAX_N_BATCH);
        let mempool = Arc::new(IntentMempool::new(pool.clone(), c.chain_id));
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
        pending_nullifiers: Arc::new(pending_nullifiers),
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
