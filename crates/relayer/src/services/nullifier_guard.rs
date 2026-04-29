//! Pre-SNARK nullifier validation.
//!
//! Two layers:
//!   1. Per-chain in-memory `HashSet<[u8; 32]>` of nullifiers currently
//!      being processed by this relayer process. Rejects concurrent
//!      duplicate submissions before SNARK proof generation.
//!   2. `spent_nullifiers` DB lookup. Rejects replays of already-mined
//!      nullifiers (indexer-populated; eventually consistent with chain).
//!
//! Both checks run before the spend/swap pipeline takes the tree lock, so
//! a doomed submission never wastes CPU on Groth16 or gas on a revert.

use crate::domain::error::{AppError, AppResult};
use crate::repositories::spent_nullifiers;
use database::DbPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

pub type PendingMap = Arc<HashMap<i64, Arc<Mutex<HashSet<[u8; 32]>>>>>;

/// Reserve nullifiers in the per-chain pending set and verify they are
/// not already on-chain. Returned guard removes the entries on drop, so
/// reservations are released on success, error, or panic alike.
pub async fn reserve_and_check(
    pending: &PendingMap,
    pool: &DbPool,
    chain_id: i64,
    nfs: [[u8; 32]; 2],
) -> AppResult<PendingGuard> {
    let set = pending
        .get(&chain_id)
        .ok_or(AppError::UnknownChain(chain_id))?
        .clone();

    // Layer 1: in-flight check + reservation. Hold the lock across
    // contains+insert so two concurrent identical requests cannot both
    // pass through the gap.
    {
        let mut s = set.lock().await;
        if s.contains(&nfs[0]) || s.contains(&nfs[1]) {
            return Err(AppError::NullifierInFlight(format!(
                "chain {} nullifier already in flight",
                chain_id
            )));
        }
        s.insert(nfs[0]);
        s.insert(nfs[1]);
    }
    let guard = PendingGuard {
        set: set.clone(),
        nfs,
    };

    // Layer 2: DB check (against indexer-populated spent_nullifiers).
    // On any failure or hit, `guard` drops and releases the reservation.
    let hits = spent_nullifiers::any_spent(pool, chain_id, &nfs).await?;
    if !hits.is_empty() {
        return Err(AppError::NullifierAlreadySpent(format!(
            "chain {} ({} hit)",
            chain_id,
            hits.len()
        )));
    }
    Ok(guard)
}

/// RAII handle. Removes both reserved nullifiers from the per-chain
/// pending set when dropped, regardless of the caller's outcome.
pub struct PendingGuard {
    set: Arc<Mutex<HashSet<[u8; 32]>>>,
    nfs: [[u8; 32]; 2],
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let set = self.set.clone();
        let nfs = self.nfs;
        // try_lock fast path avoids spawning when uncontended (common).
        if let Ok(mut s) = set.try_lock() {
            s.remove(&nfs[0]);
            s.remove(&nfs[1]);
            return;
        }
        tokio::spawn(async move {
            let mut s = set.lock().await;
            s.remove(&nfs[0]);
            s.remove(&nfs[1]);
        });
    }
}
