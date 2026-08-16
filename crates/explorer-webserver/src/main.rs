use anyhow::{Context, Result};
use explorer_webserver::adapters::PriceClient;
use explorer_webserver::app::cache::AppCache;
use explorer_webserver::{AppState, ExplorerWebserverConfig, build_info, build_router};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    build_info::log_banner();

    let cfg = ExplorerWebserverConfig::from_env()?;

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::webserver())
        .await
        .context("build pool")?;
    let cache = AppCache::new(cfg.cache_ttl_s, cfg.price_ttl_s);
    let prices = PriceClient::new(
        cfg.price_base_url.clone(),
        Duration::from_millis(cfg.price_timeout_ms),
    )
    .context("build price client")?;
    let state = AppState {
        pool,
        cfg: Arc::new(cfg.clone()),
        cache,
        prices: Arc::new(prices),
    };
    let app = build_router(state);

    let (trigger, mut shutdown) = shared::shutdown::channel();
    tokio::spawn(shared::shutdown::watch_signals(trigger));

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    info!(addr = %cfg.bind_addr, "explorer-webserver listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.recv().await })
        .await?;
    Ok(())
}
