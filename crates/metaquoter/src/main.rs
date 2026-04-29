use anyhow::{Context, Result};
use metaquoter::{MetaQuoterConfig, build_router, build_state};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();

    let mut cfg: MetaQuoterConfig =
        shared::config::load_toml("METAQUOTER_CONFIG", "metaquoter.toml")
            .context("load metaquoter config")?;
    cfg.apply_env_overlay();

    let state = build_state(&cfg).await.context("build app state")?;

    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr)
        .await
        .with_context(|| format!("bind {}", cfg.listen_addr))?;
    info!(addr = %cfg.listen_addr, "metaquoter listening");

    axum::serve(listener, build_router(state))
        .await
        .context("axum serve")
}
