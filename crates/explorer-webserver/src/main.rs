use anyhow::{Context, Result};
use explorer_webserver::app::cache::AppCache;
use explorer_webserver::{AppState, ExplorerWebserverConfig, build_info, build_router};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    build_info::log_banner();

    let cfg = ExplorerWebserverConfig {
        database_url: std::env::var("DATABASE_URL").context("DATABASE_URL")?,
        bind_addr: std::env::var("EXPLORER_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3002".into()),
        cache_ttl_s: std::env::var("CACHE_TTL_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30),
    };

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::webserver())
        .await
        .context("build pool")?;
    let cache = AppCache::new(cfg.cache_ttl_s);
    let state = AppState {
        pool,
        cfg: Arc::new(cfg.clone()),
        cache,
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    info!(addr = %cfg.bind_addr, "explorer-webserver listening");
    axum::serve(listener, app).await?;
    Ok(())
}
