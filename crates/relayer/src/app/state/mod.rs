//! The running relayer's shared state, and how it is built from config.

mod chain;

use crate::app::config::RelayerConfig;
use crate::domain::error::{AppError, AppResult};
use crate::services::admission::idempotency::IdempotencyCache;
use crate::services::admission::nullifier_guard::NullifierGuards;
use crate::services::events::EventBroadcaster;
use crate::services::pipeline::batcher::Batcher;
use crate::services::pipeline::{FlushPipeline, SpendPipeline, SwapPipeline};
use ::asset_registry::AssetRegistry;
use chain::{Shared, build_chain};
use database::DbPool;
use groth16::TreeUpdateBatchProver;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    /// One spend pipeline per chain. `/v1/spend` looks one up by
    /// `payload.chain_id`.
    pub spend_pipelines: Arc<HashMap<i64, Arc<SpendPipeline>>>,
    /// Built only for chains with a configured `swap_wrapper_address`. `/v1/swap`
    /// looks one up by `payload.chain_id`.
    pub swap_pipelines: Arc<HashMap<i64, Arc<SwapPipeline>>>,
    /// One flush pipeline per chain, for `/v1/deposit/estimate` and the flush
    /// workers `main` spawns.
    pub flush_pipelines: Arc<HashMap<i64, Arc<FlushPipeline>>>,
    /// One batcher per chain, reachable here for the test hooks.
    pub batchers: Arc<HashMap<i64, Batcher>>,
    /// Mounts `/test/bundler/*`; see `TestHooksCfg`.
    pub test_hooks: bool,
    /// Process-wide deposit lifecycle pub/sub. The SSE handler subscribes and
    /// `FlushPipeline` publishes after each successful `flushBatch`.
    pub events: Arc<EventBroadcaster>,
    /// Database pool, used by `nullifier_guard` for `spent_nullifiers` lookups
    /// before SNARK generation.
    pub pool: DbPool,
    /// Nullifier admission control. See `services::admission::nullifier_guard`.
    pub nullifiers: Arc<NullifierGuards>,
    /// Replays a submission a caller already made under the same
    /// `Idempotency-Key`. See `services::admission::idempotency`.
    pub idempotency: Arc<IdempotencyCache>,
    /// The `assets` table, cached and shared by `/chains` and the shielded-fee
    /// check so neither reads it per request.
    pub assets: Arc<AssetRegistry>,
}

impl AppState {
    /// The spend pipeline serving `chain_id`, or a 404.
    ///
    /// Every endpoint dispatches on a caller-supplied chain id, so the unknown-
    /// chain answer lives here rather than being restated at each one.
    pub fn spend_pipeline(&self, chain_id: i64) -> AppResult<Arc<SpendPipeline>> {
        lookup(&self.spend_pipelines, chain_id)
    }

    /// The swap pipeline serving `chain_id`. Absent on chains with no
    /// `swap_wrapper_address`, which reads as the same 404 to a caller.
    pub fn swap_pipeline(&self, chain_id: i64) -> AppResult<Arc<SwapPipeline>> {
        lookup(&self.swap_pipelines, chain_id)
    }

    /// The flush pipeline serving `chain_id`.
    pub fn flush_pipeline(&self, chain_id: i64) -> AppResult<Arc<FlushPipeline>> {
        lookup(&self.flush_pipelines, chain_id)
    }

    /// The batcher serving `chain_id`, for the test hooks.
    pub fn batcher(&self, chain_id: i64) -> AppResult<Batcher> {
        lookup(&self.batchers, chain_id)
    }

    pub fn serves_chain(&self, chain_id: i64) -> bool {
        self.spend_pipelines.contains_key(&chain_id)
    }
}

fn lookup<T: Clone>(map: &HashMap<i64, T>, chain_id: i64) -> AppResult<T> {
    map.get(&chain_id)
        .cloned()
        .ok_or(AppError::UnknownChain(chain_id))
}

pub async fn build_state(
    cfg: &RelayerConfig,
    pool: DbPool,
    prover: Arc<dyn TreeUpdateBatchProver>,
) -> AppResult<AppState> {
    let shared = Shared::new(cfg, pool.clone(), prover)?;

    let mut spend_pipelines = HashMap::new();
    let mut swap_pipelines = HashMap::new();
    let mut flush_pipelines = HashMap::new();
    let mut batchers = HashMap::new();
    // Built concurrently rather than one after another: each chain's mirror
    // bootstrap is a database read and an RPC `currentRoot()`, and the chains
    // share nothing that a build mutates. Boot then costs the slowest chain
    // rather than their sum. Registration below stays sequential and in config
    // order, so a restart still logs the same sequence.
    //
    // Each future carries its own `ChainCfg` back rather than the loop re-pairing
    // by position: the config is what supplies the `chain_id` every pipeline is
    // registered under, and a mispairing would serve one chain's tree from
    // another's.
    let built = futures::future::try_join_all(cfg.chains.iter().map(async |c| {
        let runtime = build_chain(c, &shared).await?;
        Ok::<_, AppError>((c, runtime))
    }))
    .await?;
    for (c, chain) in built {
        batchers.insert(c.chain_id, chain.batcher);
        flush_pipelines.insert(c.chain_id, chain.flush);
        spend_pipelines.insert(c.chain_id, chain.spend);
        if let Some(swap) = chain.swap {
            swap_pipelines.insert(c.chain_id, swap);
        }
    }

    Ok(AppState {
        spend_pipelines: Arc::new(spend_pipelines),
        swap_pipelines: Arc::new(swap_pipelines),
        flush_pipelines: Arc::new(flush_pipelines),
        batchers: Arc::new(batchers),
        test_hooks: cfg.test_hooks.enabled,
        events: shared.events,
        pool,
        nullifiers: Arc::new(NullifierGuards::new(cfg.chains.iter().map(|c| c.chain_id))),
        idempotency: Arc::new(IdempotencyCache::new()),
        assets: shared.assets,
    })
}
