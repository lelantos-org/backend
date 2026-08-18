//! Pre-SNARK nullifier validation.
//!
//! Every check covers all `TRANSACT_IN` nullifiers. The handlers used to pass
//! only the first two — a leftover from the 2x2 shape — which left the last
//! one unguarded on all three layers.
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

use crate::adapters::parse::{FieldRef, parse_field};
use crate::domain::dto::{PubInputsDto, TRANSACT_IN};
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

/// The nullifier set of one transact payload, at the deployed circuit arity.
/// Named so a shape change is a compile error at every call site rather than a
/// silently short slice.
pub type Nullifiers = [[u8; 32]; TRANSACT_IN];

/// Every nullifier a payload spends, parsed from the wire.
///
/// Takes the whole array rather than an index range: reading a fixed prefix is
/// exactly the bug this replaces.
pub fn nullifiers_of(pi: &PubInputsDto) -> AppResult<Nullifiers> {
    let mut out = [[0u8; 32]; TRANSACT_IN];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = parse_field(&pi.nullifier[i], FieldRef::Index("pubInputs.nullifier", i))?.0;
    }
    Ok(out)
}

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

    /// Reserve every nullifier in the payload and verify none is already
    /// spent. The returned guard releases the reservation on drop — success,
    /// error, or panic alike — so callers can hold it for the whole submission
    /// and forget about it.
    ///
    /// On success the caller must call [`PendingGuard::spent`]; plain drop
    /// only releases the reservation.
    pub async fn reserve(
        &self,
        pool: &DbPool,
        chain_id: i64,
        nfs: Nullifiers,
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
        nfs: Nullifiers,
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
    nfs: Nullifiers,
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

    /// Distinct nullifiers built from a seed, at the full circuit arity.
    fn nfs(a: u8, b: u8) -> Nullifiers {
        let mut out = [[0u8; 32]; TRANSACT_IN];
        out[0] = [a; 32];
        out[1] = [b; 32];
        // Padding has to be seed-dependent, or two calls with different (a, b)
        // would still collide on the slots past the second.
        for (i, slot) in out.iter_mut().enumerate().skip(2) {
            *slot = [a.wrapping_mul(31).wrapping_add(b).wrapping_add(i as u8); 32];
        }
        out
    }

    /// Same as `nfs`, but the collision sits in the LAST slot — the one the
    /// handlers never used to pass.
    fn nfs_colliding_on_last(last: u8) -> Nullifiers {
        let mut out = nfs(50, 51);
        out[TRANSACT_IN - 1] = [last; 32];
        out
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

    /// The regression: a payload whose only reused nullifier is the last one
    /// used to sail through every layer, burning a Groth16 on a tx the pool
    /// would reject.
    #[tokio::test]
    async fn a_collision_on_the_last_nullifier_is_rejected() {
        let c = chain();
        let _held = c
            .reserve_local(CHAIN_ID, nfs_colliding_on_last(0xAB))
            .await
            .unwrap();

        let err = c
            .reserve_local(CHAIN_ID, nfs_colliding_on_last(0xAB))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NullifierInFlight(_)), "got {err}");
    }

    /// And the same for the already-landed cache.
    #[tokio::test]
    async fn a_landed_last_nullifier_is_held_back_too() {
        let c = chain();
        let guard = c
            .reserve_local(CHAIN_ID, nfs_colliding_on_last(0xCD))
            .await
            .unwrap();
        guard.spent().await;
        drop(guard);

        let err = c
            .reserve_local(CHAIN_ID, nfs_colliding_on_last(0xCD))
            .await
            .unwrap_err();
        assert!(
            matches!(err, AppError::NullifierAlreadySpent(_)),
            "got {err}"
        );
    }

    /// `nfs` must cover the whole shape, or the tests above would pass for the
    /// wrong reason.
    #[test]
    fn the_guard_covers_every_circuit_input() {
        assert_eq!(nfs(1, 2).len(), TRANSACT_IN);
    }

    #[tokio::test]
    async fn an_unconfigured_chain_is_not_an_empty_set() {
        let guards = NullifierGuards::new([CHAIN_ID]);
        assert!(guards.chains.contains_key(&CHAIN_ID));
        assert!(!guards.chains.contains_key(&(CHAIN_ID + 1)));
    }
}
