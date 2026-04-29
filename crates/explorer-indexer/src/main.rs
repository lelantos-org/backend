use anyhow::{Context, Result};
use explorer_indexer::config::ExplorerIndexerConfig;
use explorer_indexer::services::consume::ConsumeServiceImpl;
use explorer_indexer::version;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();

    info!(
        version = version::CARGO_PKG_VERSION,
        commit = version::GIT_SHA,
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

    let svc = Arc::new(ConsumeServiceImpl::new(pool, Arc::new(cfg)));

    let (trigger, shutdown) = shared::shutdown::channel();
    let worker = tokio::spawn(shared::tick::run(svc, tick_ms, batch, shutdown));

    shared::shutdown::watch_signals(trigger).await;
    let _ = worker.await;
    Ok(())
}
