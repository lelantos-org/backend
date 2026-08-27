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

    /// Resize a preset, keeping `min_idle` in proportion.
    ///
    /// Every preset above holds `min_idle` at roughly a quarter of `max_size`.
    /// Overriding `max_size` with a struct update would leave `min_idle` at the
    /// preset's absolute value, so a pool sized up for more workers would keep
    /// warming the same two connections. Resizing belongs here, next to the
    /// ratio it has to preserve.
    pub const fn with_max_size(self, max_size: u32) -> Self {
        let max_size = if max_size == 0 { 1 } else { max_size };
        Self {
            max_size,
            // Integer division floors, so `max(1)` keeps a small pool warming
            // at least one connection rather than none.
            min_idle: Some(if max_size / 4 == 0 { 1 } else { max_size / 4 }),
            ..self
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A struct update over a preset would leave `min_idle` at the preset's
    /// absolute value, so a pool sized up for more workers would keep warming
    /// only the original two connections.
    #[test]
    fn resizing_keeps_min_idle_in_proportion() {
        let cfg = PoolCfg::indexer().with_max_size(32);
        assert_eq!(cfg.max_size, 32);
        assert_eq!(cfg.min_idle, Some(8));
    }

    #[test]
    fn resizing_preserves_the_preset_timeouts() {
        let base = PoolCfg::indexer();
        let cfg = base.with_max_size(16);
        assert_eq!(cfg.connection_timeout, base.connection_timeout);
        assert_eq!(cfg.idle_timeout, base.idle_timeout);
    }

    /// bb8 rejects a zero pool, and a pool that warms nothing pays a connect on
    /// every first checkout.
    #[test]
    fn a_tiny_pool_still_warms_one_connection() {
        let cfg = PoolCfg::indexer().with_max_size(1);
        assert_eq!(cfg.max_size, 1);
        assert_eq!(cfg.min_idle, Some(1));
    }
}
