//! Replay protection for submissions, keyed by the caller's `Idempotency-Key`.
//!
//! A spend is not safe to send twice. The wallet knows this, and sends one key
//! per submission, holding it across its own retries — so a request that was
//! received but whose response never got home arrives again under the same
//! key. Without this, the second arrival is indistinguishable from a fresh
//! double-spend: the nullifier guard refuses it with a 409, and a caller who
//! only ever saw a timeout is told their spend conflicts with itself.
//!
//! Keyed per chain, since a key is only unique to the wallet that minted it.
//!
//! One process, deliberately: this mirrors `nullifier_guard`, and both are
//! sound for the same reason — the tree mirror is per-process state, so two
//! relayers cannot serve the same chain anyway.

use crate::domain::error::{AppError, AppResult};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::info;

/// How long an answered key is replayable. Matches the nullifier guard's
/// recently-spent window: past it, a resubmit is caught by the spent set
/// instead, which is the honest answer once the indexer has the nullifier.
const TTL: Duration = Duration::from_secs(15 * 60);

/// Most keys held at once. A key costs a hash and a transaction hash, so this
/// is sized for headroom rather than against memory.
const CAPACITY: u64 = 10_000;

/// Idempotent submission results, keyed by `(chain_id, key)`.
pub struct IdempotencyCache {
    entries: moka::future::Cache<(i64, String), String>,
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self {
            entries: moka::future::Cache::builder()
                .max_capacity(CAPACITY)
                .time_to_live(TTL)
                .build(),
        }
    }

    /// Run `submit` once per key, and hand every later caller of that key the
    /// same transaction hash.
    ///
    /// Concurrent callers collapse onto one run: the retry that races the
    /// original — the wallet gave up waiting while the relayer was still
    /// proving — waits for it rather than being refused as a duplicate.
    ///
    /// Failures are not recorded. A submission that did not land leaves the
    /// key free, so a client that retries after a genuine failure is served
    /// rather than handed back the error forever.
    ///
    /// `key` is `None` when the caller sent no header, which runs `submit`
    /// with no replay protection at all — the pre-header behaviour.
    pub async fn run<F>(&self, chain_id: i64, key: Option<String>, submit: F) -> AppResult<String>
    where
        F: Future<Output = AppResult<String>>,
    {
        let Some(key) = key else {
            return submit.await;
        };

        // A replay is otherwise invisible: the caller gets a transaction hash
        // and the logs show no submission behind it. `try_get_with` does not
        // say whether it ran the future, so the future says so itself.
        let ran = AtomicBool::new(false);
        let result = self
            .entries
            .try_get_with((chain_id, key), async {
                ran.store(true, Ordering::SeqCst);
                submit.await
            })
            .await
            .map_err(|shared: Arc<AppError>| AppError::mirrored(&shared));

        if let Ok(tx_hash) = &result
            && !ran.load(Ordering::SeqCst)
        {
            info!(chain_id, tx_hash, "replayed an earlier submission");
        }
        result
    }
}

impl Default for IdempotencyCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    const CHAIN_ID: i64 = 31337;

    fn key(k: &str) -> Option<String> {
        Some(k.to_string())
    }

    #[tokio::test]
    async fn a_repeat_of_an_answered_key_replays_the_first_answer() {
        let cache = IdempotencyCache::new();
        let runs = AtomicUsize::new(0);
        let submit = || async {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok("0xabc".to_string())
        };

        assert_eq!(
            cache.run(CHAIN_ID, key("k"), submit()).await.unwrap(),
            "0xabc"
        );
        assert_eq!(
            cache.run(CHAIN_ID, key("k"), submit()).await.unwrap(),
            "0xabc"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1, "second call re-submitted");
    }

    /// The case the wallet actually hits: it stops waiting and retries while
    /// the relayer is still proving the first attempt.
    #[tokio::test]
    async fn a_retry_that_races_the_original_waits_for_it() {
        let cache = Arc::new(IdempotencyCache::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Barrier::new(2));

        let call = |cache: Arc<IdempotencyCache>, runs: Arc<AtomicUsize>, gate: Arc<Barrier>| async move {
            cache
                .run(CHAIN_ID, key("k"), async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    // Hold the first run open until both callers are inside.
                    gate.wait().await;
                    Ok("0xabc".to_string())
                })
                .await
        };

        let first = tokio::spawn(call(cache.clone(), runs.clone(), gate.clone()));
        // The second caller must not start its own run, so only the first can
        // reach the barrier — release it from here.
        let second = tokio::spawn({
            let cache = cache.clone();
            let runs = runs.clone();
            async move {
                cache
                    .run(CHAIN_ID, key("k"), async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        Ok("other".to_string())
                    })
                    .await
            }
        });
        gate.wait().await;

        assert_eq!(first.await.unwrap().unwrap(), "0xabc");
        assert_eq!(second.await.unwrap().unwrap(), "0xabc");
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the retry ran its own submit"
        );
    }

    #[tokio::test]
    async fn a_failed_submission_leaves_the_key_free() {
        let cache = IdempotencyCache::new();

        let err = cache
            .run(CHAIN_ID, key("k"), async {
                Err(AppError::Reverted("out of gas".into()))
            })
            .await
            .unwrap_err();
        assert_eq!(err.status(), AppError::Reverted(String::new()).status());

        let ok = cache
            .run(CHAIN_ID, key("k"), async { Ok("0xabc".to_string()) })
            .await
            .unwrap();
        assert_eq!(ok, "0xabc");
    }

    #[tokio::test]
    async fn the_same_key_on_another_chain_is_a_different_submission() {
        let cache = IdempotencyCache::new();
        cache
            .run(CHAIN_ID, key("k"), async { Ok("0xabc".to_string()) })
            .await
            .unwrap();
        let other = cache
            .run(CHAIN_ID + 1, key("k"), async { Ok("0xdef".to_string()) })
            .await
            .unwrap();
        assert_eq!(other, "0xdef");
    }

    #[tokio::test]
    async fn without_a_key_every_call_submits() {
        let cache = IdempotencyCache::new();
        let runs = AtomicUsize::new(0);
        for _ in 0..2 {
            cache
                .run(CHAIN_ID, None, async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok("0xabc".to_string())
                })
                .await
                .unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
