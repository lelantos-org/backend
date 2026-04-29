use anyhow::{Context, Result};
use fmd_webserver::app::cache::AppCache;
use fmd_webserver::{AppState, FmdWebserverConfig, build_info, build_router};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    build_info::log_banner();

    let cfg = FmdWebserverConfig::from_env()?;
    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::webserver())
        .await
        .context("build pool")?;
    let state = AppState {
        pool,
        cfg: Arc::new(cfg.clone()),
        cache: AppCache::new(),
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    info!(addr = %cfg.bind_addr, "fmd-webserver listening");
    axum::serve(listener, app).await?;
    Ok(())
}
