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

    axum::serve(listener, build_router(state))
        .await
        .context("axum serve")
}
