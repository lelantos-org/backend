use anyhow::{Context, Result};
use risk_webserver::{RiskWebserverConfig, build_info, build_router, build_state};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    build_info::log_banner();

    let cfg = RiskWebserverConfig::from_env()?;

    // Unlike the other webservers this one runs migrations: no indexer touches
    // `screened_addresses`, so nothing else would create it.
    let migrate_url = cfg.database_url.clone();
    tokio::task::spawn_blocking(move || database::migrate::run(&migrate_url))
        .await
        .context("join migrate task")?
        .context("run migrations")?;

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::webserver())
        .await
        .context("build pool")?;
    let state = build_state(Arc::new(cfg.clone()), pool);

    let (trigger, mut shutdown) = shared::shutdown::channel();
    tokio::spawn(shared::shutdown::watch_signals(trigger));

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    info!(addr = %cfg.bind_addr, "risk-webserver listening");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(async move { shutdown.recv().await })
        .await?;
    Ok(())
}
