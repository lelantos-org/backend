use anyhow::{Context, Result};
use relayer::services::prover::{Groth16Prover, TreeUpdateBatchProver};
use relayer::{RelayerConfig, build_router, build_state};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();

    let mut cfg: RelayerConfig = shared::config::load_toml("RELAYER_CONFIG", "relayer.toml")
        .context("load relayer config")?;
    cfg.apply_env_overlay();
    // After the overlay, so env-supplied values are checked too.
    cfg.validate().context("relayer config")?;

    // Migrations are idempotent, and run here in case the ingester starts after
    // the relayer in a compose dependency graph.
    {
        let url = cfg.database_url.clone();
        tokio::task::spawn_blocking(move || database::migrate::run(&url))
            .await
            .context("migrate spawn_blocking")?
            .context("migrate")?;
    }

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::relayer())
        .await
        .context("build pool")?;

    info!(
        graph = %cfg.prover.graph_path.display(),
        zkey = %cfg.prover.zkey_path.display(),
        "groth16 prover loading",
    );
    let prover: Arc<dyn TreeUpdateBatchProver> = Arc::new(
        Groth16Prover::new(&cfg.prover.graph_path, &cfg.prover.zkey_path)
            .context("groth16 prover init")?,
    );

    let state = build_state(&cfg, pool, prover)
        .await
        .context("build app state")?;

    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr)
        .await
        .with_context(|| format!("bind {}", cfg.listen_addr))?;
    info!(addr = %cfg.listen_addr, "relayer listening");

    // Graceful shutdown: a rolling restart mid-submission would drop the caller's
    // connection while its transaction may be in flight, leaving them unable to
    // distinguish a failed spend from a landed one.
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shared::shutdown::signal())
        .await
        .context("axum serve")
}
