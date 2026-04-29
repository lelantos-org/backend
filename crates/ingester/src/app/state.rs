use crate::adapters::DynRpc;
use crate::app::config::ChainConfig;
use crate::repositories::{ChainStateRepo, RawEventRepo};
use crate::services::backfill::BackfillService;
use crate::services::ingest::IngestService;
use crate::services::reorg::ReorgService;
use std::sync::Arc;

/// Bag of dependencies for one chain worker. Built once in `main`.
pub struct WorkerDeps {
    pub cfg: ChainConfig,
    pub rpc: DynRpc,
    pub raw_events: Arc<dyn RawEventRepo>,
    pub chain_state: Arc<dyn ChainStateRepo>,
    pub ingest: Arc<IngestService>,
    pub reorg: Arc<ReorgService>,
    pub backfill: Arc<BackfillService>,
}
