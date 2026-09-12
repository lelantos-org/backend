use anyhow::{Context, Result};
use rpc_proxy::{RpcProxyConfig, build_info, build_router, build_state};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();
    build_info::log_banner();

    let mut cfg: RpcProxyConfig = shared::config::load_toml("RPC_PROXY_CONFIG", "rpc-proxy.toml")
        .context("load rpc-proxy config")?;
    cfg.apply_env_overlay()
        .context("apply rpc-proxy env overlay")?;

    shared::metrics::init_addr(&cfg.metrics_addr).context("init metrics")?;

    let state = build_state(&cfg).await.context("build app state")?;

    // Reclaims the rate limiter's idle buckets, which nothing else does, and
    // publishes the occupancy of every bound this service leans on.
    tokio::spawn(rpc_proxy::services::housekeeping::run(state.clone()));

    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr)
        .await
        .with_context(|| format!("bind {}", cfg.listen_addr))?;
    info!(addr = %cfg.listen_addr, chains = state.chains.len(), "rpc-proxy listening");

    // `into_make_service_with_connect_info` rather than a plain service: the
    // rate limiter keys on the peer address when no trusted header is
    // configured, and without this the socket address is not available to the
    // handler at all.
    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shared::shutdown::signal())
    .await
    .context("axum serve")
}
