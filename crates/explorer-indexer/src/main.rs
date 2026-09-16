use anyhow::{Context, Result};
use explorer_indexer::adapters::ChainLocks;
use explorer_indexer::app::build_info;
use explorer_indexer::app::config::ExplorerIndexerConfig;
use explorer_indexer::services::consume::ConsumeServiceImpl;
use std::sync::Arc;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();

    info!(
        version = build_info::PKG_VERSION,
        commit = build_info::GIT_SHA,
        "explorer-indexer starting"
    );

    let cfg: ExplorerIndexerConfig =
        shared::config::load_toml("EXPLORER_INDEXER_CONFIG", "explorer-indexer.toml")
            .context("load config")?;
    let tick_ms = cfg.tick_ms;
    let batch = cfg.batch;

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::indexer())
        .await
        .context("build pool")?;

    info!(tick_ms, batch, "explorer-indexer ready");

    // One service, and no chain client: this binary reads rows the ingester has
    // already written and aggregates them. Everything needing an RPC — ERC20
    // metadata and the yield-index poll — belongs to protocol-indexer.
    //
    // Locked per chain, so N replicas are failover rather than scale-out: a
    // standby skips its tick instead of re-running the window and the view
    // rebuild the leader is already doing.
    let locks = Arc::new(ChainLocks::new(&cfg.database_url));
    let consume = Arc::new(ConsumeServiceImpl::new(pool, locks));

    let (trigger, shutdown) = shared::shutdown::channel();
    let worker = tokio::spawn(shared::tick::run(consume, tick_ms, batch, shutdown));

    shared::shutdown::watch_signals(trigger).await;
    // Joined with the outcome checked, not discarded. A tick that panics unwinds
    // its driver, and `let _ =` swallowed that: the service died, nothing was
    // logged, and the process stayed up and exited 0 — a silently half-running
    // indexer is worse than one that fell over.
    if let Err(e) = worker.await {
        error!(service = "explorer", error = %e, "tick worker terminated abnormally");
    }
    Ok(())
}
