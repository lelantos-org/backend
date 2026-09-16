use anyhow::{Context, Result, anyhow};
use ingester::adapters::HttpRpc;
use ingester::app::build_info;
use ingester::app::config::{IngesterConfig, redact_url};
use ingester::app::state::WorkerDeps;
use ingester::handlers::worker::supervisor;
use shared::shutdown;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    info!(
        version = build_info::PKG_VERSION,
        commit = build_info::GIT_SHA,
        "ingester starting"
    );

    let cfg = load_config()?;

    shared::metrics::init_addr(&cfg.metrics_addr)?;

    info!("running migrations");
    database::migrate::run_locked(&cfg.database_url)
        .await
        .context("migrations")?;
    info!("migrations complete");

    let pool_cfg = cfg.pool();
    let pool = database::build_pool(&cfg.database_url, pool_cfg)
        .await
        .context("build pool")?;
    info!(
        pool_size = pool_cfg.max_size,
        chains = cfg.chains.len(),
        "db pool built"
    );

    let (trigger, shutdown) = shutdown::channel();
    tokio::spawn(shutdown::watch_signals(trigger));

    let mut deps = Vec::with_capacity(cfg.chains.len());
    for chain_cfg in cfg.chains {
        info!(
            chain_id = chain_cfg.chain_id,
            rpc_url = %redact_url(&chain_cfg.rpc_url),
            "spawning worker"
        );
        let rpc = HttpRpc::build(&(&chain_cfg).into())?;
        deps.push(WorkerDeps::new(&pool, chain_cfg, rpc, &cfg.database_url));
    }

    let workers = supervisor::spawn(deps, &shutdown);
    supervisor::await_all(workers)
        .await
        .map_err(|failed| anyhow!("chain workers failed: {:?}", failed))
}

fn load_config() -> Result<IngesterConfig> {
    let mut cfg: IngesterConfig = shared::config::load_toml("INGESTER_CONFIG", "ingester.toml")
        .context("load ingester config")?;
    cfg.apply_env_overlay().context("apply env overlay")?;
    // Validated before anything is spawned, so a bad address or a zero chunk size
    // fails the process now rather than after a standby has waited out a chain
    // lock.
    cfg.validate().context("validate ingester config")?;
    info!(chains = cfg.chains.len(), "config loaded");
    Ok(cfg)
}
