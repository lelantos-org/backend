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
