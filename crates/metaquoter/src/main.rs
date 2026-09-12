use anyhow::{Context, Result};
use metaquoter::{MetaQuoterConfig, build_info, build_router, build_state};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    build_info::log_banner();

    let mut cfg: MetaQuoterConfig =
        shared::config::load_toml("METAQUOTER_CONFIG", "metaquoter.toml")
            .context("load metaquoter config")?;
    cfg.apply_env_overlay()
        .context("apply metaquoter env overlay")?;

    let state = build_state(&cfg).await.context("build app state")?;

    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr)
        .await
        .with_context(|| format!("bind {}", cfg.listen_addr))?;
    info!(addr = %cfg.listen_addr, "metaquoter listening");

    // In-flight quotes finish rather than being cut mid-`eth_call`: a caller
    // whose connection drops has no way to tell a rollout from a venue failure
    // and will retry into the replica that is still coming up.
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shared::shutdown::signal())
        .await
        .context("axum serve")
}
