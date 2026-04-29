use anyhow::{Context, Result};
use ingester::adapters::{DynRpc, HttpRpc};
use ingester::app::config::IngesterConfig;
use ingester::app::state::WorkerDeps;
use ingester::handlers::worker;
use ingester::repositories::{PostgresChainStateRepo, PostgresRawEventRepo};
use ingester::services::backfill::BackfillService;
use ingester::services::ingest::IngestService;
use ingester::services::reorg::ReorgService;
use ingester::version;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    info!(
        version = version::CARGO_PKG_VERSION,
        commit = version::GIT_SHA,
        "ingester starting"
    );

    let mut cfg: IngesterConfig = shared::config::load_toml("INGESTER_CONFIG", "ingester.toml")
        .context("load ingester config")?;
    cfg.apply_env_overlay();
    info!(chains = cfg.chains.len(), "config loaded");

    let url = cfg.database_url.clone();
    info!("running migrations");
    tokio::task::spawn_blocking(move || database::migrate::run(&url))
        .await?
        .context("migrations")?;
    info!("migrations complete");

    let pool_cfg = database::PoolCfg::indexer();
    let pool = database::build_pool(&cfg.database_url, pool_cfg)
        .await
        .context("build pool")?;
    info!(pool_size = pool_cfg.max_size, "db pool built");

    let mut handles = Vec::new();
    for chain_cfg in cfg.chains {
        let chain_id = chain_cfg.chain_id;
        info!(chain_id, "spawning worker");

        let rpc: DynRpc = HttpRpc::build(&chain_cfg.rpc_url)?;
        let raw_events = Arc::new(PostgresRawEventRepo::new(pool.clone()));
        let chain_state = Arc::new(PostgresChainStateRepo::new(pool.clone()));
        let ingest = Arc::new(IngestService::new(raw_events.clone(), chain_state.clone()));
        let reorg = Arc::new(ReorgService::new(raw_events.clone(), chain_state.clone()));
        let backfill = Arc::new(BackfillService::new(rpc.clone(), ingest.clone()));

        let deps = WorkerDeps {
            cfg: chain_cfg,
            rpc,
            raw_events,
            chain_state,
            ingest,
            reorg,
            backfill,
        };

        let h = tokio::spawn(async move {
            if let Err(e) = worker::run(deps).await {
                tracing::error!(chain_id, "worker exited: {}", e);
            }
        });
        handles.push(h);
    }

    for h in handles {
        let _ = h.await;
    }
    Ok(())
}
