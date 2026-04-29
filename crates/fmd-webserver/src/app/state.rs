use crate::app::cache::AppCache;
use crate::app::config::FmdWebserverConfig;
use database::DbPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub cfg: Arc<FmdWebserverConfig>,
    pub cache: AppCache,
}
