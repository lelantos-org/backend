use bb8::Pool;
use diesel_async::AsyncPgConnection;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use std::time::Duration;
use thiserror::Error;

pub type DbPool = Pool<AsyncDieselConnectionManager<AsyncPgConnection>>;
/// A connection checked out of [`DbPool`]. Named so repositories can hand it
/// around without spelling the bb8/diesel-async generics at every call site.
pub type DbConn<'a> = bb8::PooledConnection<'a, AsyncDieselConnectionManager<AsyncPgConnection>>;

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("bb8 build: {0}")]
    Build(String),
}

#[derive(Debug, Clone, Copy)]
pub struct PoolCfg {
    pub max_size: u32,
    pub min_idle: Option<u32>,
    pub connection_timeout: Duration,
    pub idle_timeout: Option<Duration>,
}

impl PoolCfg {
    pub const fn webserver() -> Self {
        Self {
            max_size: 32,
            min_idle: Some(8),
            connection_timeout: Duration::from_secs(5),
            idle_timeout: Some(Duration::from_secs(60 * 10)),
        }
    }

    pub const fn indexer() -> Self {
        Self {
            max_size: 8,
            min_idle: Some(2),
            connection_timeout: Duration::from_secs(5),
            idle_timeout: Some(Duration::from_secs(60 * 10)),
        }
    }

    pub const fn relayer() -> Self {
        Self {
            max_size: 4,
            min_idle: Some(1),
            connection_timeout: Duration::from_secs(5),
            idle_timeout: Some(Duration::from_secs(60 * 10)),
        }
    }
}

impl Default for PoolCfg {
    fn default() -> Self {
        Self::indexer()
    }
}

pub async fn build_pool(database_url: &str, cfg: PoolCfg) -> Result<DbPool, PoolError> {
    let mgr = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    Pool::builder()
        .max_size(cfg.max_size)
        .min_idle(cfg.min_idle)
        .connection_timeout(cfg.connection_timeout)
        .idle_timeout(cfg.idle_timeout)
        .build(mgr)
        .await
        .map_err(|e| PoolError::Build(e.to_string()))
}
