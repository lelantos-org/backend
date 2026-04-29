use crate::domain::error::FmdIndexerError;
use database::{DbPool, PoolCfg};

pub async fn build_pool(database_url: &str, cfg: PoolCfg) -> Result<DbPool, FmdIndexerError> {
    database::build_pool(database_url, cfg)
        .await
        .map_err(|e| FmdIndexerError::Db(e.to_string()))
}
