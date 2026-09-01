use std::time::Duration;

use diesel::Connection;
use diesel::pg::PgConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use thiserror::Error;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");

#[derive(Debug, Error)]
pub enum MigrateError {
    #[error("connect: {0}")]
    Connect(#[from] diesel::ConnectionError),
    #[error("migration: {0}")]
    Run(String),
    #[error("migration lock: {0}")]
    Lock(String),
}

/// Runs migrations using a synchronous connection. Call once at process startup
/// from a blocking context (`tokio::task::spawn_blocking`).
pub fn run(database_url: &str) -> Result<(), MigrateError> {
    // Runs under the caller's session-scoped migration lock and inside a long
    // DDL transaction, so it takes the same direct path as that lock rather
    // than a pooled connection. See `crate::direct`.
    let url = crate::direct::url(database_url);
    let mut conn = PgConnection::establish(&url)?;
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| MigrateError::Run(e.to_string()))?;
    Ok(())
}

/// Poll interval while another process holds the migration lock.
const LOCK_POLL: Duration = Duration::from_secs(1);
/// Attempts before giving up on the migration lock (two minutes at [`LOCK_POLL`]).
const LOCK_ATTEMPTS: u32 = 120;

/// Runs migrations under the schema-migration advisory lock.
///
/// `diesel_migrations` takes no lock of its own, so processes booting together
/// would otherwise apply the same migration concurrently and collide in the
/// catalog (`duplicate key value violates unique constraint
/// "pg_type_typname_nsp_index"`). Every binary that migrates at startup must
/// go through here rather than calling [`run`] directly; the losers wait and
/// then find nothing pending.
pub async fn run_locked(database_url: &str) -> Result<(), MigrateError> {
    let lock = acquire_lock(database_url).await?;
    let url = database_url.to_string();
    let result = tokio::task::spawn_blocking(move || run(&url)).await;
    // Held until here: dropping the lock closes its connection and releases it.
    drop(lock);
    result.map_err(|e| MigrateError::Run(format!("join migrate task: {e}")))?
}

async fn acquire_lock(database_url: &str) -> Result<crate::ChainLock, MigrateError> {
    for attempt in 0..LOCK_ATTEMPTS {
        match crate::ChainLock::try_acquire(database_url, crate::advisory::MIGRATE_KEY).await {
            Ok(Some(lock)) => return Ok(lock),
            Ok(None) => {
                if attempt == 0 {
                    tracing::info!("another process is migrating; waiting");
                }
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(e) => return Err(MigrateError::Lock(e.to_string())),
        }
    }
    Err(MigrateError::Lock(
        "timed out waiting for the migration lock".to_string(),
    ))
}
