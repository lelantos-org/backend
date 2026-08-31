use anyhow::{Context, Result};
use explorer_indexer::adapters::masp::{DynMaspYieldReader, HttpMaspYieldReader};
use explorer_indexer::adapters::{DynTokenMetadata, HttpTokenMetadata};
use explorer_indexer::build_info;
use explorer_indexer::config::ExplorerIndexerConfig;
use explorer_indexer::services::consume::ConsumeServiceImpl;
use explorer_indexer::services::yield_state::YieldStateServiceImpl;
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
    // and the yield index stay unresolved, so a bad URL must not abort startup.
    // Both readers parse the same URL, so in practice they fail together.
    let mut token_meta: HashMap<i64, DynTokenMetadata> = HashMap::new();
    let mut yield_readers: HashMap<i64, DynMaspYieldReader> = HashMap::new();
    for c in &cfg.chains {
        match HttpTokenMetadata::build(&c.rpc_url) {
            Ok(rpc) => {
                token_meta.insert(c.chain_id, rpc as DynTokenMetadata);
            }
            Err(e) => warn!(chain_id = c.chain_id, "token metadata RPC disabled: {}", e),
        }
        match HttpMaspYieldReader::build(&c.rpc_url) {
            Ok(rpc) => {
                yield_readers.insert(c.chain_id, rpc as DynMaspYieldReader);
            }
            Err(e) => warn!(chain_id = c.chain_id, "yield state RPC disabled: {}", e),
        }
    }
    let token_meta = Arc::new(token_meta);
    let yield_readers = Arc::new(yield_readers);

    let pool = database::build_pool(&cfg.database_url, database::PoolCfg::indexer())
        .await
        .context("build pool")?;

    info!(tick_ms, batch, "explorer-indexer ready");

    let consume = Arc::new(ConsumeServiceImpl::new(
        pool.clone(),
        Arc::new(cfg),
        token_meta,
    ));
    let yield_state = Arc::new(YieldStateServiceImpl::new(pool, yield_readers));

    let (trigger, shutdown) = shared::shutdown::channel();
    let consume_worker = tokio::spawn(shared::tick::run(consume, tick_ms, batch, shutdown.clone()));
    let yield_worker = tokio::spawn(shared::tick::run(yield_state, tick_ms, batch, shutdown));

    shared::shutdown::watch_signals(trigger).await;
    let _ = consume_worker.await;
    let _ = yield_worker.await;
    Ok(())
}
