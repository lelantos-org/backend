use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::repositories::ChainStateRepo;
use crate::services::backfill::BackfillService;
use crate::services::ingest::IngestService;
use crate::services::reorg::ReorgService;
use std::sync::Arc;

/// Bag of dependencies for one chain worker. Built once in `main`.
///
/// `Clone` so the supervisor can restart a worker without rebuilding the
/// provider and repositories — everything inside is either `Arc` or cheap.
#[derive(Clone)]
pub struct WorkerDeps {
    pub cfg: ChainConfig,
    pub rpc: DynRpc,
    pub chain_state: Arc<dyn ChainStateRepo>,
    pub ingest: Arc<IngestService>,
    pub reorg: Arc<ReorgService>,
    pub backfill: Arc<BackfillService>,
    /// For the advisory lock's dedicated connection — it must not come from
    /// the shared pool, or `idle_timeout` would reap it and silently release
    /// the lock. See `database::advisory`.
    pub database_url: String,
}
