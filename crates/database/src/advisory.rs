//! Cross-process per-chain mutual exclusion via Postgres advisory locks.
//!
//! Lets a service be deployed as N replicas where exactly one is active per
//! chain and the rest are hot standby: losers skip their tick and retry, so a
//! dead leader is taken over automatically once its lock releases.
//!
//! The lock is **session-level** (`pg_try_advisory_lock`), not transaction
//! scoped — it has to outlive individual statements, and the indexers issue
//! standalone autocommit statements rather than transactions.
//!
//! Because the lock lives on the session, it is held on a **dedicated
//! connection that never enters the bb8 pool**. A pooled connection would be
//! returned after the query and eventually reaped by `idle_timeout`
//! (`PoolCfg::indexer` uses 10 min), silently releasing the lock while the
//! process keeps working — two writers, no error. Owning the connection ties
//! the lock's lifetime to the `ChainLock` value instead.

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
/// Namespace for `fmd-indexer`'s per-chain consume locks.
///
/// Distinct from [`NS_INGESTER`] so the two services can each hold their own
/// lock for the same chain — they guard different tables and must not exclude
/// one another.
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

/// A held per-chain advisory lock. Dropping it closes the connection, which
/// releases the lock — so graceful shutdown hands over to a standby promptly.
pub struct ChainLock {
    conn: AsyncPgConnection,
    key: i64,
}

impl ChainLock {
    /// Try to take the lock for `key`. `Ok(None)` means another process holds
    /// it; that is the normal standby path, not an error.
    pub async fn try_acquire(database_url: &str, key: i64) -> AdvisoryResult<Option<Self>> {
        let mut conn = AsyncPgConnection::establish(database_url)
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

    /// Round-trip the lock connection to confirm the session — and therefore
    /// the lock — is still alive.
    ///
    /// Without this a dropped connection leaves the holder believing it is
    /// still the leader while a standby acquires the freed lock: split brain,
    /// which is the exact failure the lock exists to prevent. Callers must
    /// check this before each unit of work and drop the `ChainLock` on false.
    pub async fn is_alive(&mut self) -> bool {
        diesel::sql_query("SELECT 1 AS one")
            .get_result::<Alive>(&mut self.conn)
            .await
            .is_ok_and(|r| r.one == 1)
    }
}
