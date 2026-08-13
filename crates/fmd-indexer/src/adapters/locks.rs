//! Per-chain leader election for the consume loop.
//!
//! Lets fmd-indexer run as N replicas: each tick, a replica must hold the
//! advisory lock for a chain before touching it. One replica wins, the rest
//! skip and retry, and a dead leader's lock releases so a standby takes over.
//!
//! This is failover, not scale-out — a chain is still processed by exactly one
//! process at a time.

use crate::domain::error::{FmdIndexerError, Result};
use database::advisory::{ChainLock, NS_FMD_CONSUME, chain_key};
use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Locks held by this process, keyed by chain. Acquired lazily on first tick
/// for a chain and then held for process lifetime.
pub struct ChainLocks {
    database_url: Option<String>,
    held: Mutex<HashMap<i64, ChainLock>>,
}

impl ChainLocks {
    pub fn enabled(database_url: impl Into<String>) -> Self {
        Self {
            database_url: Some(database_url.into()),
            held: Mutex::new(HashMap::new()),
        }
    }

    /// No locking — every caller believes it is the leader.
    ///
    /// Only for single-process tests. In a deployment this reintroduces the
    /// concurrent-writer races the locks exist to prevent, including silent
    /// `spent_nullifiers.seq` gaps.
    pub fn disabled() -> Self {
        Self {
            database_url: None,
            held: Mutex::new(HashMap::new()),
        }
    }

    /// Whether this process may act on `chain_id` right now.
    ///
    /// Re-checks the lock connection on every call: if it has died, the lock
    /// is gone and another replica may already have taken over, so continuing
    /// to write would be split brain. Drop the guard and re-acquire next tick.
    pub async fn is_leader(&self, chain_id: i64) -> Result<bool> {
        let Some(url) = &self.database_url else {
            return Ok(true);
        };
        let mut held = self.held.lock().await;

        if let Some(lock) = held.get_mut(&chain_id) {
            if lock.is_alive().await {
                return Ok(true);
            }
            warn!(chain_id, "lock connection died; releasing leadership");
            held.remove(&chain_id);
            return Ok(false);
        }

        let key = chain_key(NS_FMD_CONSUME, chain_id);
        match ChainLock::try_acquire(url, key).await {
            Ok(Some(lock)) => {
                info!(chain_id, "consume lock acquired; acting as leader");
                held.insert(chain_id, lock);
                Ok(true)
            }
            // Standby. Debug, not info: at a 500ms tick this fires twice a
            // second per chain on every non-leader replica.
            Ok(None) => {
                debug!(chain_id, "consume lock held elsewhere; standing by");
                Ok(false)
            }
            Err(e) => Err(FmdIndexerError::Db(e.to_string())),
        }
    }
}
