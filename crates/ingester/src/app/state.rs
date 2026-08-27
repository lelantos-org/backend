use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::repositories::ChainStateRepo;
use crate::services::backfill::BackfillService;
use crate::services::ingest::IngestService;
use crate::services::log_range::LogWindow;
use crate::services::reorg::ReorgService;
use std::sync::Arc;

/// Bag of dependencies for one chain worker. Built once in `main`.
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
