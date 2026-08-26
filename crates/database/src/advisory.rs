//! Cross-process per-chain mutual exclusion via Postgres advisory locks.
//!
//! Lets a service run as N replicas with exactly one active per chain and the
//! rest on hot standby: losers skip their tick and retry, so a dead leader is
//! taken over once its lock releases.
//!
//! The lock is session-level (`pg_try_advisory_lock`) rather than transaction
//! scoped, because it must outlive individual statements and the indexers
//! issue standalone autocommit statements.
//!
//! Since the lock lives on the session, it is held on a dedicated connection
//! that never enters the bb8 pool. A pooled connection would be returned after
//! the query and eventually reaped by `idle_timeout` (`PoolCfg::indexer` uses
//! 10 min), releasing the lock while the process keeps working. Owning the
//! connection ties the lock's lifetime to the `ChainLock` value.

use diesel::QueryableByName;
use diesel::sql_types::{BigInt, Bool};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdvisoryError {
    #[error("connect: {0}")]
    Connect(String),
    #[error("query: {0}")]
    Query(#[from] diesel::result::Error),
}

pub type AdvisoryResult<T> = Result<T, AdvisoryError>;

/// Namespace for `ingester`'s per-chain worker locks.
pub const NS_INGESTER: i64 = 0x1A95_0000_0000_0000_u64 as i64;
/// Namespace for the schema-migration lock.
///
/// Every replica runs `migrate::run` at startup and `diesel_migrations` takes
/// no lock of its own, so replicas booting together would otherwise apply the
/// same migration concurrently. Serialising here makes the losers wait and
/// then find nothing pending.
pub const NS_MIGRATE: i64 = 0x1A95_0002_0000_0000_u64 as i64;
/// Key for the single, chain-independent migration lock.
pub const MIGRATE_KEY: i64 = NS_MIGRATE;

/// Namespace for `fmd-indexer`'s per-chain consume locks.
///
/// Distinct from [`NS_INGESTER`] so both services can hold a lock for the same
/// chain: they guard different tables and must not exclude one another.
pub const NS_FMD_CONSUME: i64 = 0x1A95_0001_0000_0000_u64 as i64;

/// Advisory-lock key for one (namespace, chain) pair.
pub fn chain_key(namespace: i64, chain_id: i64) -> i64 {
    namespace ^ chain_id
}

#[derive(QueryableByName)]
struct LockAcquired {
    #[diesel(sql_type = Bool)]
    pg_try_advisory_lock: bool,
}

#[derive(QueryableByName)]
struct Alive {
    #[diesel(sql_type = BigInt)]
    one: i64,
}

/// A held per-chain advisory lock. Dropping it closes the connection and
/// releases the lock, so graceful shutdown hands over to a standby promptly.
pub struct ChainLock {
    conn: AsyncPgConnection,
    key: i64,
}

impl ChainLock {
    /// Try to take the lock for `key`. `Ok(None)` means another process holds
    /// it, which is the standby path rather than an error.
    pub async fn try_acquire(database_url: &str, key: i64) -> AdvisoryResult<Option<Self>> {
        // The lock is session-scoped, so it must not be multiplexed by a
        // pooler. See `crate::direct`.
        let url = crate::direct::url(database_url);
        let mut conn = AsyncPgConnection::establish(&url)
            .await
            .map_err(|e| AdvisoryError::Connect(e.to_string()))?;
        let got: LockAcquired =
            diesel::sql_query("SELECT pg_try_advisory_lock($1) AS pg_try_advisory_lock")
                .bind::<BigInt, _>(key)
                .get_result(&mut conn)
                .await?;
        Ok(got.pg_try_advisory_lock.then_some(Self { conn, key }))
    }

    pub fn key(&self) -> i64 {
        self.key
    }

    /// Round-trip the lock connection to confirm the session, and therefore
    /// the lock, is still alive.
    ///
    /// A dropped connection would otherwise leave the holder acting as leader
    /// while a standby acquires the freed lock. Callers must check this before
    /// each unit of work and drop the `ChainLock` on `false`.
    pub async fn is_alive(&mut self) -> bool {
        // The cast is required: a bare `1` is `integer` in Postgres and
        // deserializing int4 into `BigInt`/i64 fails. Such an error is
        // indistinguishable from a dead connection, so an uncast literal would
        // make a healthy session report as dead.
        diesel::sql_query("SELECT 1::bigint AS one")
            .get_result::<Alive>(&mut self.conn)
            .await
            .is_ok_and(|r| r.one == 1)
    }
}
