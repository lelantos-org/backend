use anyhow::{Context, Result};
use fmd_indexer::adapters;
use fmd_indexer::adapters::locks::ChainLocks;
use fmd_indexer::app::{self, FmdIndexerConfig};
use fmd_indexer::handlers::worker;
use fmd_indexer::repositories::cursor::PostgresCursorRepo;
use fmd_indexer::repositories::matches::PostgresMatchesRepo;
use fmd_indexer::repositories::notes::PostgresNotesRepo;
use fmd_indexer::repositories::raw_events::PostgresRawEventsRepo;
use fmd_indexer::repositories::spent_nullifiers::PostgresSpentNullifiersRepo;
use fmd_indexer::repositories::subscriptions::PostgresSubscriptionsRepo;
use fmd_indexer::services::{ConsumeServiceImpl, FilterServiceImpl};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    shared::tracing_init::init();

    info!(
        version = app::version::CARGO_PKG_VERSION,
        commit = app::version::GIT_SHA,
        "fmd-indexer starting"
    );

    let cfg = FmdIndexerConfig::load().context("load config")?;

    #[cfg(feature = "parallel")]
    rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.filter_workers)
        .build_global()
        .ok();

    let pool = adapters::db::build_pool(&cfg.database_url, database::PoolCfg::indexer())
        .await
        .context("build pool")?;

    let cursors = Arc::new(PostgresCursorRepo::new(pool.clone()));
    let raw_events = Arc::new(PostgresRawEventsRepo::new(pool.clone()));
    let notes = Arc::new(PostgresNotesRepo::new(pool.clone()));
    let spent_nfs = Arc::new(PostgresSpentNullifiersRepo::new(pool.clone()));
    let subscriptions = Arc::new(PostgresSubscriptionsRepo::new(pool.clone()));
    let matches = Arc::new(PostgresMatchesRepo::new(pool.clone()));

    let consume = Arc::new(ConsumeServiceImpl::new(
        pool.clone(),
        cursors.clone(),
        raw_events,
        notes.clone(),
        spent_nfs,
        ChainLocks::enabled(&cfg.database_url),
    ));
    let filter = Arc::new(FilterServiceImpl::new(
        cursors,
        notes,
        subscriptions,
        matches,
    ));

    info!(workers = cfg.filter_workers, "fmd-indexer ready");

    let (trigger, shutdown) = app::shutdown::channel();

    let consume_handle = tokio::spawn(worker::consume::run(
        consume,
        cfg.consume_tick_ms(),
        cfg.consume_batch() as i64,
        shutdown.clone(),
    ));
    let filter_handle = tokio::spawn(worker::filter::run(
        filter,
        cfg.filter_tick_ms,
        cfg.filter_batch as i64,
        shutdown.clone(),
    ));

    app::shutdown::watch_signals(trigger).await;

    let _ = tokio::join!(consume_handle, filter_handle);
    Ok(())
}
