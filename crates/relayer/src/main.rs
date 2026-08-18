use anyhow::{Context, Result};
use relayer::services::prover::{ArkCircomProver, TreeUpdateBatchProver};
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
    // After the overlay, so an env-supplied value is checked too.
    cfg.validate().context("relayer config")?;

    // Migrations are idempotent; run here in case the ingester comes up after
    // the relayer in compose dependency graphs.
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
        wasm = %cfg.prover.wasm_path.display(),
        r1cs = %cfg.prover.r1cs_path.display(),
        zkey = %cfg.prover.zkey_path.display(),
        "ark-circom prover loading",
    );
    let prover: Arc<dyn TreeUpdateBatchProver> = Arc::new(
        ArkCircomProver::new(
            &cfg.prover.wasm_path,
            &cfg.prover.r1cs_path,
            &cfg.prover.zkey_path,
        )
        .context("ark-circom prover init")?,
    );

    let state = build_state(&cfg, pool, prover)
        .await
        .context("build app state")?;

    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr)
        .await
        .with_context(|| format!("bind {}", cfg.listen_addr))?;
    info!(addr = %cfg.listen_addr, "relayer listening");

    // Graceful shutdown: a rolling restart mid-submission would otherwise drop
    // the caller's connection while its transaction may already be in flight,
    // leaving them unable to tell a failed spend from a landed one.
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")
}

/// Resolve on SIGINT or SIGTERM. In-flight requests are allowed to finish;
/// new connections are refused.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => tracing::error!(error = %e, "SIGTERM handler unavailable"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    info!("shutdown signal received; draining in-flight requests");
}
