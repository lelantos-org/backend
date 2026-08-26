use anyhow::{Context, Result};
use explorer_indexer::adapters::{DynTokenMetadata, HttpTokenMetadata};
use explorer_indexer::build_info;
use explorer_indexer::config::ExplorerIndexerConfig;
use explorer_indexer::services::consume::ConsumeServiceImpl;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();

    info!(
        version = build_info::PKG_VERSION,
        commit = build_info::GIT_SHA,
        "explorer-indexer starting"
    );

    let mut cfg: ExplorerIndexerConfig =
        shared::config::load_toml("EXPLORER_INDEXER_CONFIG", "explorer-indexer.toml")
            .context("load config")?;
    cfg.apply_env_overlay();
    let tick_ms = cfg.tick_ms;
    let batch = cfg.batch;

    // A chain with no usable RPC still indexes events; only ERC20 `decimals`
    // stays unresolved, so a bad URL must not abort startup.
    let mut token_meta: HashMap<i64, DynTokenMetadata> = HashMap::new();
    for c in &cfg.chains {
        match HttpTokenMetadata::build(&c.rpc_url) {
            Ok(rpc) => {
                token_meta.insert(c.chain_id, rpc as DynTokenMetadata);
            }
            Err(e) => warn!(chain_id = c.chain_id, "token metadata RPC disabled: {}", e),
        }
    }
    let token_meta = Arc::new(token_meta);

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::indexer())
        .await
        .context("build pool")?;

    info!(tick_ms, batch, "explorer-indexer ready");

    let svc = Arc::new(ConsumeServiceImpl::new(pool, Arc::new(cfg), token_meta));

    let (trigger, shutdown) = shared::shutdown::channel();
    let worker = tokio::spawn(shared::tick::run(svc, tick_ms, batch, shutdown));

    shared::shutdown::watch_signals(trigger).await;
    let _ = worker.await;
    Ok(())
}
