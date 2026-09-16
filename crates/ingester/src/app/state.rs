//! What one chain worker is handed at startup.

use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::repositories::{
    ChainStateRepo, PostgresAtomicWriteRepo, PostgresBlockHashRepo, PostgresChainStateRepo,
};
use crate::services::backfill::BackfillService;
use crate::services::ingest::IngestService;
use crate::services::log_range::LogWindow;
use crate::services::reorg::ReorgService;
use database::DbPool;
use std::sync::Arc;

/// Bag of dependencies for one chain worker. Built once per chain in `main`.
///
/// `Clone` so the supervisor can restart a worker without rebuilding the provider
/// and repositories; every field is an `Arc` or cheap to copy.
#[derive(Clone)]
pub struct WorkerDeps {
    pub cfg: ChainConfig,
    pub rpc: DynRpc,
    pub chain_state: Arc<dyn ChainStateRepo>,
    pub ingest: Arc<IngestService>,
    pub reorg: Arc<ReorgService>,
    pub backfill: Arc<BackfillService>,
    /// The provider's `eth_getLogs` cap, learned once and shared by the live tail
    /// and the backfill. One per chain, since it describes one provider.
    pub log_window: Arc<LogWindow>,
    /// For the advisory lock's dedicated connection, which must not come from the
    /// shared pool: `idle_timeout` would reap it and release the lock. See
    /// `database::advisory`.
    pub database_url: String,
}

impl WorkerDeps {
    /// Wire one chain's repositories and services over `pool` and `rpc`.
    ///
    /// Takes the provider rather than building it, so a test can hand in a
    /// scripted chain and still run the wiring `main` runs.
    pub fn new(pool: &DbPool, cfg: ChainConfig, rpc: DynRpc, database_url: &str) -> Self {
        let writes = Arc::new(PostgresAtomicWriteRepo::new(pool.clone()));
        let raw_events = Arc::new(PostgresBlockHashRepo::new(pool.clone()));
        let chain_state = Arc::new(PostgresChainStateRepo::new(pool.clone()));
        let ingest = Arc::new(IngestService::new(writes.clone(), chain_state.clone()));
        let reorg = Arc::new(ReorgService::new(writes, raw_events));
        let log_window = Arc::new(LogWindow::new(cfg.log_concurrency));
        let backfill = Arc::new(BackfillService::new(
            rpc.clone(),
            ingest.clone(),
            log_window.clone(),
        ));
        Self {
            cfg,
            rpc,
            chain_state,
            ingest,
            reorg,
            backfill,
            log_window,
            database_url: database_url.to_string(),
        }
    }
}
