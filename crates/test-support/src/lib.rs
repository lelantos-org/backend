//! Postgres harness for the workspace's database-backed tests.
//!
//! One container per test binary, migrated once, plus a process-wide lock that
//! serialises the tests sharing it. Tests run against a real schema rather than
//! a mock, so a migration that breaks a query fails here.

use database::{DbPool, PoolCfg};
use diesel_async::RunQueryDsl;
use std::sync::{Arc, OnceLock};
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::sync::{Mutex, OnceCell, OwnedMutexGuard};

/// How long to wait for the container's Postgres to accept connections.
const READY_ATTEMPTS: u32 = 30;
const READY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

struct Container {
    /// Held for the process lifetime; dropping it stops the container.
    _container: ContainerAsync<Postgres>,
    url: String,
}

async fn container() -> &'static Container {
    static CELL: OnceLock<OnceCell<Container>> = OnceLock::new();
    CELL.get_or_init(OnceCell::new)
        .get_or_init(|| async {
            let container = Postgres::default().start().await.expect("start postgres");
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("container port");
            let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

            // `start()` returns once the container is up, which is before Postgres
            // itself accepts connections.
            for _ in 0..READY_ATTEMPTS {
                if database::build_pool(&url, PoolCfg::indexer()).await.is_ok() {
                    break;
                }
                tokio::time::sleep(READY_INTERVAL).await;
            }

            let migrate_url = url.clone();
            tokio::task::spawn_blocking(move || database::migrate::run(&migrate_url))
                .await
                .expect("migrate join")
                .expect("migrate");

            Container {
                _container: container,
                url,
            }
        })
        .await
}

/// Connection string of this binary's container, migrated and ready.
pub async fn db_url() -> &'static str {
    &container().await.url
}

/// Serialise the tests in this binary against the one container they share.
///
/// Held for the test's duration: [`fresh_pool`] truncates, so two tests running
/// concurrently would clear each other's rows.
pub async fn serial_lock() -> OwnedMutexGuard<()> {
    static LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

/// A pool over an empty database, plus the guard serialising this test.
///
/// `tables` are truncated with `RESTART IDENTITY CASCADE`, so ids are comparable
/// across tests. The guard must stay alive for as long as the pool is used.
pub async fn fresh_pool(cfg: PoolCfg, tables: &[&str]) -> (DbPool, OwnedMutexGuard<()>) {
    let guard = serial_lock().await;
    let pool = database::build_pool(db_url().await, cfg)
        .await
        .expect("build pool");
    truncate(&pool, tables).await;
    (pool, guard)
}

async fn truncate(pool: &DbPool, tables: &[&str]) {
    if tables.is_empty() {
        return;
    }
    let mut conn = pool.get().await.expect("checkout connection");
    let sql = format!(
        "TRUNCATE TABLE {} RESTART IDENTITY CASCADE",
        tables.join(", ")
    );
    diesel::sql_query(sql)
        .execute(&mut conn)
        .await
        .expect("truncate");
}
