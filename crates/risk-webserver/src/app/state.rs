use crate::app::config::RiskWebserverConfig;
use crate::repositories::screened_addresses::PgScreenedAddressRepo;
use crate::services::screening::ScreeningService;
use database::DbPool;
use std::sync::Arc;

/// Carries no `pool` field: the pool is owned by the repo and never reached
/// from a handler.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<RiskWebserverConfig>,
    pub screening: Arc<ScreeningService>,
}

pub fn build_state(cfg: Arc<RiskWebserverConfig>, pool: DbPool) -> AppState {
    let repo = Arc::new(PgScreenedAddressRepo::new(pool));
    let screening = Arc::new(ScreeningService::new(repo, cfg.cache_ttl_s));
    AppState { cfg, screening }
}
