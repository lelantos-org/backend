use anyhow::{Context, Result};
use asset_registry::AssetRegistry;
use registry_webserver::handlers::worker::venue_apy;
use registry_webserver::{RegistryConfig, build_info, build_router, build_state};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    build_info::log_banner();

    let cfg = RegistryConfig::load()?;
    // Installs the recorder the `track_http` layer feeds; without it the
    // instrumentation compiles but exports nothing.
    shared::metrics::init_addr(&cfg.metrics_addr)?;

    // No migrations: every table this reads is owned by protocol-indexer, which
    // creates them. Running them here would let a webserver decide the shape of
    // another service's tables.
    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::webserver())
        .await
        .context("build pool")?;

    let cfg = Arc::new(cfg);
    let state = build_state(cfg.clone(), pool.clone())?;

    // The write path: one measurement worker per chain, each electing a single
    // replica for itself. See `handlers::worker::venue_apy`.
    let assets = Arc::new(AssetRegistry::new(pool.clone()));
    venue_apy::spawn_all(&cfg, pool, assets);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr)
        .await
        .with_context(|| format!("bind {}", cfg.bind_addr))?;
    info!(addr = %cfg.bind_addr, "registry-webserver listening");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shared::shutdown::signal())
        .await?;
    Ok(())
}
