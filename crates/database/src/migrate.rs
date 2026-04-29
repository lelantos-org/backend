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
    let mut conn = PgConnection::establish(database_url)?;
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| MigrateError::Run(e.to_string()))?;
    Ok(())
}
