//! Per-chain leader election for the consume loop.
//!
//! Lets explorer-indexer run as N replicas: each tick, a replica must hold the
//! advisory lock for a chain before touching it. One replica wins, the rest skip
//! and retry, and a dead leader's lock releases so a standby takes over.
//!
//! This provides failover rather than scale-out; a chain is processed by exactly
//! one process at a time.
//!
//! Without it two replicas fetch the same window, write the same rows, and both
//! rebuild the same materialized views — a whole-table aggregate run twice per
//! tick. The writes are idempotent and `upsert_monotonic` refuses the slower
//! cursor advance, so the duplicate work was never *incorrect*; it was simply
//! paid for twice, and the view rebuild is the expensive half.

use crate::domain::error::{ExplorerIndexerError, Result};
use database::advisory::{ChainLock, NS_EXPLORER_CONSUME, chain_key};
use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Locks held by this process, keyed by chain. Acquired lazily on first tick
/// for a chain and then held for process lifetime.
pub struct ChainLocks {
    database_url: String,
    held: Mutex<HashMap<i64, ChainLock>>,
}

impl ChainLocks {
    pub fn new(database_url: impl Into<String>) -> Self {
        Self {
            database_url: database_url.into(),
            held: Mutex::new(HashMap::new()),
        }
    }

    /// Whether this process may act on `chain_id` right now.
    ///
    /// Re-checks the lock connection on every call: if it has died the lock is
    /// gone and another replica may already have taken over, so continuing to
    /// write would be a split brain. The guard is dropped and re-acquired on the
    /// next tick.
    pub async fn is_leader(&self, chain_id: i64) -> Result<bool> {
        let mut held = self.held.lock().await;

        if let Some(lock) = held.get_mut(&chain_id) {
            if lock.is_alive().await {
                return Ok(true);
            }
            warn!(chain_id, "lock connection died; releasing leadership");
            held.remove(&chain_id);
            return Ok(false);
        }

        let key = chain_key(NS_EXPLORER_CONSUME, chain_id);
        match ChainLock::try_acquire(&self.database_url, key).await {
            Ok(Some(lock)) => {
                info!(chain_id, "consume lock acquired; acting as leader");
                held.insert(chain_id, lock);
                Ok(true)
            }
            // Standby. Logged at debug rather than info: this fires once per
            // tick per chain on every non-leader replica.
            Ok(None) => {
                debug!(chain_id, "consume lock held elsewhere; standing by");
                Ok(false)
            }
            Err(e) => Err(ExplorerIndexerError::Db(e.to_string())),
        }
    }
}
