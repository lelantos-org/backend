use anyhow::{Context, Result};
use explorer_webserver::adapters::{DefiLlama, PriceService};
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

    let cfg = Arc::new(ExplorerWebserverConfig::from_env()?);
    // Installs the recorder the `track_http` layer feeds; without it the
    // instrumentation compiles but exports nothing.
    shared::metrics::init_addr(&cfg.metrics_addr)?;

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::webserver())
        .await
        .context("build pool")?;
    let cache = AppCache::new(cfg.cache_ttl_s);
    let prices = PriceService::new(
        // One provider today; a second one is another entry in this vector.
        vec![Arc::new(
            DefiLlama::new(
                &cfg.price_base_url,
                Duration::from_millis(cfg.price_timeout_ms),
            )
            .context("build price client")?,
        )],
        Duration::from_secs(cfg.price_ttl_s),
    );
    let state = AppState {
        pool,
        cfg: Arc::clone(&cfg),
        cache,
        prices: Arc::new(prices),
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    info!(addr = %cfg.bind_addr, "explorer-webserver listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shared::shutdown::signal())
        .await?;
    Ok(())
}
