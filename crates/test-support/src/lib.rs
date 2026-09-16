//! Postgres harness for the workspace's database-backed tests.
//!
//! Every test process shares one Postgres container. The schema is migrated once
//! into a template database, and each process works in its own clone of it.
//! Tests run against a real schema rather than a mock, so a migration that breaks
//! a query fails here.

pub mod fixtures;

use database::{DbPool, PoolCfg};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use testcontainers::core::Mount;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt, ReuseDirective};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::{Mutex, OnceCell, OwnedMutexGuard};

/// The version production runs (`stack/docker-compose.yml`).
const POSTGRES_TAG: &str = "16-alpine";

/// Label the shared container is found by. Matching on a label rather than a
/// container name means a container stopped by a daemon restart does not block
/// starting a new one.
const CONTAINER_LABEL: (&str, &str) = ("org.lelantos.test-postgres", POSTGRES_TAG);

/// Durability is worthless for databases that are thrown away, and it is most
/// of what migrations cost. Under nextest every test is a process with its own
/// connection pool, hence the connection limit.
const SERVER_ARGS: &[&str] = &[
    "postgres",
    "-c",
    "fsync=off",
    "-c",
    "synchronous_commit=off",
    "-c",
    "full_page_writes=off",
    "-c",
    "max_connections=500",
];

/// Clones older than this were left behind by a killed process.
const STALE_CLONE_AGE: Duration = Duration::from_secs(3600);

/// Held for the process lifetime. A reused container is not stopped on drop.
static CONTAINER: OnceCell<ContainerAsync<Postgres>> = OnceCell::const_new();

/// Connection string of this process's database, migrated and ready.
pub async fn db_url() -> &'static str {
    static URL: OnceCell<String> = OnceCell::const_new();
    URL.get_or_init(create_clone).await
}

/// Serialise the tests in this process against the one database they share.
///
/// Held for the test's duration: [`fresh_pool`] truncates, so two tests running
/// concurrently would clear each other's rows. Under nextest each test is its
/// own process with its own database, so the lock is uncontended there.
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

/// Start or attach to the shared server, make sure the template exists, and
/// clone it for this process.
async fn create_clone() -> String {
    // Container start, template build and cleanup are serialised across every
    // test process on this machine. Only the first process after a change does
    // real work under it; the rest find everything in place.
    let lock = host_lock();
    let admin = admin_url().await;
    let mut conn = connect(&admin).await;
    let template = ensure_template(&mut conn, &admin).await;
    sweep(&mut conn, &template).await;
    drop(lock);

    let name = format!(
        "t_{}_{}_{}",
        unix_now().as_secs(),
        std::process::id(),
        unix_now().subsec_nanos()
    );
    exec(
        &mut conn,
        &format!("CREATE DATABASE {name} TEMPLATE {template}"),
    )
    .await;
    drop_at_exit(&admin, &name);
    with_database(&admin, &name)
}

/// An exclusive lock on a file in the temp directory, released on drop.
fn host_lock() -> std::fs::File {
    let path = std::env::temp_dir().join("lelantos-test-postgres.lock");
    let file = std::fs::File::create(path).expect("create lock file");
    file.lock().expect("lock");
    file
}

async fn admin_url() -> String {
    let container = CONTAINER
        .get_or_init(|| async {
            Postgres::default()
                .with_tag(POSTGRES_TAG)
                .with_label(CONTAINER_LABEL.0, CONTAINER_LABEL.1)
                .with_reuse(ReuseDirective::Always)
                .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
                .with_cmd(SERVER_ARGS.iter().copied())
                .start()
                .await
                .expect("start postgres")
        })
        .await;
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container port");
    format!("postgres://postgres:postgres@{host}:{port}/postgres")
}

/// Connect, waiting out a server that is still starting: the image's init runs a
/// temporary server before the real one, so its ready message can come early.
async fn connect(url: &str) -> AsyncPgConnection {
    let mut last = None;
    for _ in 0..100 {
        match AsyncPgConnection::establish(url).await {
            Ok(conn) => return conn,
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("connect {url}: {last:?}");
}

/// The migrated template for the current migrations, built if missing.
///
/// Named after a hash of `crates/database/migrations`, so adding or editing a
/// migration builds a new one.
async fn ensure_template(conn: &mut AsyncPgConnection, admin: &str) -> String {
    let template = format!("tpl_{:016x}", migrations_hash());
    if !database_names(conn).await.contains(&template) {
        let build = format!("{template}_build");
        exec(conn, &format!("DROP DATABASE IF EXISTS {build}")).await;
        exec(conn, &format!("CREATE DATABASE {build}")).await;
        let url = with_database(admin, &build);
        tokio::task::spawn_blocking(move || database::migrate::run(&url))
            .await
            .expect("migrate join")
            .expect("migrate");
        // Renamed only once complete, so a failed build is never cloned.
        exec(
            conn,
            &format!("ALTER DATABASE {build} RENAME TO {template}"),
        )
        .await;
    }
    template
}

fn migrations_hash() -> u64 {
    fn visit(dir: &Path, hasher: &mut impl Hasher) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read migrations")
            .map(|e| e.expect("dir entry").path())
            .collect();
        entries.sort();
        for path in entries {
            path.file_name().hash(hasher);
            if path.is_dir() {
                visit(&path, hasher);
            } else {
                std::fs::read(&path).expect("read migration").hash(hasher);
            }
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../database/migrations");
    let mut hasher = std::hash::DefaultHasher::new();
    visit(&dir, &mut hasher);
    hasher.finish()
}

/// Drop templates of other migration sets, and clones abandoned by a killed
/// process. Clones of live processes are younger than the cutoff.
async fn sweep(conn: &mut AsyncPgConnection, template: &str) {
    let cutoff = unix_now().saturating_sub(STALE_CLONE_AGE).as_secs();
    for name in database_names(conn).await {
        let stale_template = name.starts_with("tpl_") && name != template;
        let stale_clone = clone_created_at(&name).is_some_and(|t| t < cutoff);
        if stale_template || stale_clone {
            exec(
                conn,
                &format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
            )
            .await;
        }
    }
}

/// The creation time a clone's name carries: `t_<unix secs>_<pid>_<nanos>`.
fn clone_created_at(name: &str) -> Option<u64> {
    name.strip_prefix("t_")?.split('_').next()?.parse().ok()
}

/// Drop this process's clone when it exits, so a nextest run does not leave a
/// database per test behind. A killed process skips this; [`sweep`] collects its
/// clone later.
fn drop_at_exit(admin: &str, name: &str) {
    static TARGET: OnceLock<(String, String)> = OnceLock::new();
    TARGET
        .set((admin.to_string(), name.to_string()))
        .expect("one clone per process");

    extern "C" fn drop_clone() {
        use diesel::Connection;
        let (admin, name) = TARGET.get().expect("set before registering");
        if let Ok(mut conn) = diesel::PgConnection::establish(admin) {
            let sql = format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)");
            let _ = diesel::RunQueryDsl::execute(diesel::sql_query(sql), &mut conn);
        }
    }
    // SAFETY: `drop_clone` is an `extern "C" fn()` that only reads a static.
    unsafe { libc::atexit(drop_clone) };
}

async fn database_names(conn: &mut AsyncPgConnection) -> Vec<String> {
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Text)]
        datname: String,
    }
    diesel::sql_query("SELECT datname FROM pg_database")
        .load::<Row>(conn)
        .await
        .expect("list databases")
        .into_iter()
        .map(|r| r.datname)
        .collect()
}

async fn exec(conn: &mut AsyncPgConnection, sql: &str) {
    diesel::sql_query(sql)
        .execute(conn)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// `url` with its database replaced by `name`. The harness builds every URL it
/// passes here, so they carry no query string.
fn with_database(url: &str, name: &str) -> String {
    let (server, _) = url.rsplit_once('/').expect("url has a database");
    format!("{server}/{name}")
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_database() {
        assert_eq!(
            with_database("postgres://u:p@localhost:5432/postgres", "t_1"),
            "postgres://u:p@localhost:5432/t_1"
        );
    }

    #[test]
    fn reads_a_clone_creation_time() {
        assert_eq!(clone_created_at("t_1700000000_42_7"), Some(1_700_000_000));
        assert_eq!(clone_created_at("tpl_abc"), None);
        assert_eq!(clone_created_at("postgres"), None);
    }
}
