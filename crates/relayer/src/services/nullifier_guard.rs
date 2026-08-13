//! Pre-SNARK nullifier validation.
//!
//! Three layers, cheapest first:
//!   1. Per-chain set of nullifiers currently being processed by this relayer.
//!      Rejects concurrent duplicate submissions before proof generation.
//!   2. Per-chain TTL cache of nullifiers this relayer has *already* landed
//!      on-chain. `spent_nullifiers` is written by the indexer, which trails
//!      the chain by some blocks; without this layer a resubmit inside that
//!      window passes both other checks and burns a Groth16 plus gas on a tx
//!      the contract will reject.
//!   3. `spent_nullifiers` lookup. Catches replays this process did not submit
//!      itself, and everything older than the TTL.
//!
//! All three run before the spend/swap pipeline takes the tree lock, so a
//! doomed submission never wastes CPU on Groth16 or gas on a revert.

use crate::domain::error::{AppError, AppResult};
use crate::repositories::spent_nullifiers;
use database::DbPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// How long a landed nullifier stays in the recently-spent cache. Needs to
/// comfortably exceed indexer lag — it only has to hold until the nullifier
/// reaches `spent_nullifiers`, after which layer 3 takes over.
const RECENTLY_SPENT_TTL: Duration = Duration::from_secs(15 * 60);

/// Nullifier admission control, one entry per configured chain. A chain that
/// is absent is an unknown chain, not an empty set.
pub struct NullifierGuards {
    chains: HashMap<i64, Arc<ChainNullifiers>>,
}

impl NullifierGuards {
    pub fn new(chain_ids: impl IntoIterator<Item = i64>) -> Self {
        Self {
            chains: chain_ids
                .into_iter()
                .map(|id| (id, Arc::new(ChainNullifiers::new())))
                .collect(),
        }
    }

    /// Reserve both nullifiers and verify neither is already spent. The
    /// returned guard releases the reservation on drop — success, error, or
    /// panic alike — so callers can hold it for the whole submission and
    /// forget about it.
    ///
    /// On success the caller must call [`PendingGuard::spent`]; plain drop
    /// only releases the reservation.
    pub async fn reserve(
        &self,
        pool: &DbPool,
        chain_id: i64,
        nfs: [[u8; 32]; 2],
    ) -> AppResult<PendingGuard> {
        let chain = self
            .chains
            .get(&chain_id)
            .ok_or(AppError::UnknownChain(chain_id))?;

        // Layers 1 and 2 are in-memory and cheap; do them before touching the
        // pool. `guard` exists from here on, so every later `?` releases.
        let guard = chain.reserve_local(chain_id, nfs).await?;

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
}

/// One chain's in-memory nullifier state.
#[derive(Debug)]
struct ChainNullifiers {
    in_flight: Mutex<HashSet<[u8; 32]>>,
    recently_spent: moka::future::Cache<[u8; 32], ()>,
}

impl ChainNullifiers {
    fn new() -> Self {
        Self {
            in_flight: Mutex::new(HashSet::new()),
            recently_spent: moka::future::Cache::builder()
                .time_to_live(RECENTLY_SPENT_TTL)
                .build(),
        }
    }

    /// Layers 1 and 2, no I/O.
    async fn reserve_local(
        self: &Arc<Self>,
        chain_id: i64,
        nfs: [[u8; 32]; 2],
    ) -> AppResult<PendingGuard> {
        // Hold the lock across contains+insert so two concurrent identical
        // requests cannot both pass through the gap.
        {
            let mut in_flight = self.in_flight.lock().await;
            if nfs.iter().any(|nf| in_flight.contains(nf)) {
                return Err(AppError::NullifierInFlight(format!(
                    "chain {chain_id} nullifier already in flight"
                )));
            }
            in_flight.extend(nfs);
        }
        let guard = PendingGuard {
            chain: self.clone(),
            nfs,
        };

        for nf in &nfs {
            if self.recently_spent.get(nf).await.is_some() {
                return Err(AppError::NullifierAlreadySpent(format!(
                    "chain {chain_id} (submitted by this relayer, awaiting indexer)"
                )));
            }
        }
        Ok(guard)
    }
}

/// RAII handle over a nullifier reservation.
#[derive(Debug)]
pub struct PendingGuard {
    chain: Arc<ChainNullifiers>,
    nfs: [[u8; 32]; 2],
}

impl PendingGuard {
    /// Record that these nullifiers landed on-chain, so a resubmit is rejected
    /// during the window before the indexer writes them to
    /// `spent_nullifiers`.
    pub async fn spent(&self) {
        for nf in &self.nfs {
            self.chain.recently_spent.insert(*nf, ()).await;
        }
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let chain = self.chain.clone();
        let nfs = self.nfs;
        let release = move |in_flight: &mut HashSet<[u8; 32]>| {
            for nf in &nfs {
                in_flight.remove(nf);
            }
        };
        // try_lock fast path avoids spawning when uncontended (the common case).
        if let Ok(mut in_flight) = chain.in_flight.try_lock() {
            release(&mut in_flight);
            return;
        }
        tokio::spawn(async move {
            let mut in_flight = chain.in_flight.lock().await;
            release(&mut in_flight);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAIN_ID: i64 = 31337;

    fn nfs(a: u8, b: u8) -> [[u8; 32]; 2] {
        [[a; 32], [b; 32]]
    }

    fn chain() -> Arc<ChainNullifiers> {
        Arc::new(ChainNullifiers::new())
    }

    #[tokio::test]
    async fn a_second_reservation_of_the_same_nullifier_is_rejected() {
        let c = chain();
        let _held = c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap();

        let err = c.reserve_local(CHAIN_ID, nfs(1, 9)).await.unwrap_err();
        assert!(matches!(err, AppError::NullifierInFlight(_)), "got {err}");
    }

    #[tokio::test]
    async fn disjoint_nullifiers_reserve_concurrently() {
        let c = chain();
        let _a = c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap();
        c.reserve_local(CHAIN_ID, nfs(3, 4)).await.unwrap();
    }

    /// Dropping without `spent()` is the failure path: the submission never
    /// landed, so the nullifiers must become available again.
    #[tokio::test]
    async fn dropping_a_guard_releases_the_reservation() {
        let c = chain();
        drop(c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap());
        c.reserve_local(CHAIN_ID, nfs(1, 2))
            .await
            .expect("reservation should have been released");
    }

    /// A rejected reservation must not strand the nullifiers it collided with.
    #[tokio::test]
    async fn a_rejected_reservation_leaves_the_holder_intact() {
        let c = chain();
        let held = c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap();
        assert!(c.reserve_local(CHAIN_ID, nfs(1, 9)).await.is_err());

        drop(held);
        c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap();
    }

    /// The indexer-lag window: the tx landed and the guard dropped, but
    /// `spent_nullifiers` has not caught up yet.
    #[tokio::test]
    async fn a_landed_nullifier_is_rejected_after_its_guard_drops() {
        let c = chain();
        let guard = c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap();
        guard.spent().await;
        drop(guard);

        let err = c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap_err();
        assert!(
            matches!(err, AppError::NullifierAlreadySpent(_)),
            "got {err}"
        );
    }

    /// Only the nullifiers that actually landed are held back.
    #[tokio::test]
    async fn marking_spent_does_not_block_unrelated_nullifiers() {
        let c = chain();
        let guard = c.reserve_local(CHAIN_ID, nfs(1, 2)).await.unwrap();
        guard.spent().await;
        drop(guard);

        c.reserve_local(CHAIN_ID, nfs(3, 4)).await.unwrap();
    }

    #[tokio::test]
    async fn an_unconfigured_chain_is_not_an_empty_set() {
        let guards = NullifierGuards::new([CHAIN_ID]);
        assert!(guards.chains.contains_key(&CHAIN_ID));
        assert!(!guards.chains.contains_key(&(CHAIN_ID + 1)));
    }
}
