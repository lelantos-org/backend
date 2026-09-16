use bb8::Pool;
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, PoolError as ManagerError};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
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
    /// Server-side deadline for a single statement, applied to every connection
    /// this pool opens. `None` leaves the server default, which is no limit.
    ///
    /// This is the ceiling on how long one query may hold a pooled connection.
    /// Without it a single pathological statement occupies its slot until the
    /// client goes away, and enough of them exhaust `max_size`; every other
    /// caller then fails at checkout after `connection_timeout`, so one slow
    /// endpoint takes down the cheap ones and `/health` with it. With it, the
    /// slow statement fails and the connection returns to the pool.
    pub statement_timeout: Option<Duration>,
}

impl PoolCfg {
    /// Sized for request/response traffic. The statement deadline is well above
    /// any query these serve and well below a caller's patience: a response
    /// taking longer than this is of no use to the client that asked for it, so
    /// the only thing a longer deadline buys is a held connection.
    pub const fn webserver() -> Self {
        Self {
            max_size: 32,
            min_idle: Some(8),
            connection_timeout: Duration::from_secs(5),
            idle_timeout: Some(Duration::from_secs(60 * 10)),
            statement_timeout: Some(Duration::from_secs(15)),
        }
    }

    /// Deliberately far looser than [`Self::webserver`]. An indexer's slowest
    /// legitimate statement is a `REFRESH MATERIALIZED VIEW CONCURRENTLY` over
    /// the whole of `asset_flows`, which grows with the table, and no user is
    /// waiting on it. The deadline is here to release a connection wedged on a
    /// dead socket, not to bound honest work, so it sits above anything the tick
    /// loop legitimately does.
    pub const fn indexer() -> Self {
        Self {
            max_size: 8,
            min_idle: Some(2),
            connection_timeout: Duration::from_secs(5),
            idle_timeout: Some(Duration::from_secs(60 * 10)),
            statement_timeout: Some(Duration::from_secs(300)),
        }
    }

    /// Four connections, so exhaustion is cheap to reach and expensive to sit
    /// in: a submission holds its chain's tree-mirror mutex across the DB work,
    /// and every spend queued behind it waits. The deadline is tighter than the
    /// webserver's for that reason — nothing on this path is an analytic query.
    pub const fn relayer() -> Self {
        Self {
            max_size: 4,
            min_idle: Some(1),
            connection_timeout: Duration::from_secs(5),
            idle_timeout: Some(Duration::from_secs(60 * 10)),
            statement_timeout: Some(Duration::from_secs(30)),
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

/// Applies [`PoolCfg::statement_timeout`] to each connection as it is opened.
///
/// bb8 calls `on_acquire` once per physical connection, immediately after the
/// manager connects — not on every checkout — so this costs one extra round trip
/// per connection established, not one per query.
///
/// ⚠️ Session state, and therefore subject to the caveat in [`crate::direct`]: a
/// transaction pooler multiplexes many clients onto shared server connections,
/// where a `SET` issued outside a transaction lands on whichever server
/// connection was assigned and does not follow this client. A deployment running
/// pooled traffic through PgDog must configure the timeout on the pooler
/// instead; this then applies only to the direct connections.
#[derive(Debug)]
struct StatementTimeout(Duration);

#[async_trait::async_trait]
impl bb8::CustomizeConnection<AsyncPgConnection, ManagerError> for StatementTimeout {
    async fn on_acquire(&self, conn: &mut AsyncPgConnection) -> Result<(), ManagerError> {
        // `SET` takes no bind parameters, so the value is interpolated. It is a
        // `u128` rendered from our own `Duration`, never caller input.
        //
        // Milliseconds because `statement_timeout`'s bare-integer form is
        // milliseconds; a value under 1 ms would truncate to 0, which Postgres
        // reads as "no limit" — the opposite of what was asked — so it is
        // floored at 1.
        let ms = self.0.as_millis().max(1);
        diesel::sql_query(format!("SET statement_timeout = {ms}"))
            .execute(conn)
            .await
            .map(|_| ())
            .map_err(ManagerError::QueryError)
    }
}

pub async fn build_pool(database_url: &str, cfg: PoolCfg) -> Result<DbPool, PoolError> {
    let mgr = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    let mut builder = Pool::builder()
        .max_size(cfg.max_size)
        .min_idle(cfg.min_idle)
        .connection_timeout(cfg.connection_timeout)
        .idle_timeout(cfg.idle_timeout);
    if let Some(timeout) = cfg.statement_timeout {
        builder = builder.connection_customizer(Box::new(StatementTimeout(timeout)));
    }
    builder
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
        assert_eq!(cfg.statement_timeout, base.statement_timeout);
    }

    /// Every preset must bound a statement. A pool without one lets a single
    /// query hold its slot indefinitely, which is what exhausts `max_size` and
    /// turns one slow endpoint into a service-wide checkout failure.
    #[test]
    fn every_preset_bounds_a_statement() {
        for cfg in [PoolCfg::webserver(), PoolCfg::indexer(), PoolCfg::relayer()] {
            let timeout = cfg.statement_timeout.expect("preset bounds a statement");
            // Above the checkout deadline: a statement cancelled sooner than a
            // waiter gives up would fail requests that were never slow.
            assert!(timeout > cfg.connection_timeout, "{cfg:?}");
        }
    }

    /// The request path must not hold a connection for longer than a caller
    /// waits, while an indexer's refresh legitimately runs for minutes.
    #[test]
    fn the_request_path_is_bounded_tighter_than_the_tick_loop() {
        assert!(PoolCfg::webserver().statement_timeout < PoolCfg::indexer().statement_timeout);
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
