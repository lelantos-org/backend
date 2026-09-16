//! The state every handler is given: the pool, the config, the response caches
//! and the price service.

use crate::app::cache::AppCache;
use crate::app::config::ExplorerWebserverConfig;
use database::DbPool;
use prices::PriceService;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub cfg: Arc<ExplorerWebserverConfig>,
    pub cache: AppCache,
    pub prices: Arc<PriceService>,
}
