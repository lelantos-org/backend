//! Shared backoff policy for transient RPC and database failures.
//!
//! Before this existed, a single 429 propagated all the way out of the worker
//! task and that chain simply stopped ingesting until the process was
//! restarted — the error taxonomy in [`crate::domain::error`] was classified
//! and then never acted on.

use crate::domain::error::IngesterError;
use rand::Rng;
use std::future::Future;
use std::time::Duration;
use tracing::warn;

/// How hard to try, and how long to wait between attempts.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Total attempts, including the first. Exhausting them returns the last
    /// error rather than looping forever, which is what lets a wedged chain
    /// release its advisory lock so a standby can take over.
    pub max_attempts: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Policy {
    const BASE: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(60);

    /// One live tick. Short-lived, so a handful of attempts is plenty.
    pub const LIVE_TICK: Self = Self {
        max_attempts: 8,
        base: Self::BASE,
        max: Self::MAX,
    };

    /// A whole backfill pass. Committed chunks keep their cursor, so a retry
    /// resumes where it stopped rather than starting over.
    pub const BACKFILL: Self = Self {
        max_attempts: 8,
        base: Self::BASE,
        max: Self::MAX,
    };

    /// Restarting a chain worker from the supervisor.
    pub const WORKER_RESTART: Self = Self {
        max_attempts: 10,
        base: Self::BASE,
        max: Self::MAX,
    };

    /// Exponential backoff with full jitter, capped at [`Policy::max`].
    ///
    /// The jitter matters with replicas: without it every standby wakes on the
    /// same schedule and they retry the failing provider in lockstep.
    pub fn delay(&self, attempt: u32) -> Duration {
        let exp = self.base.saturating_mul(1u32 << attempt.min(16));
        exp.min(self.max)
            .mul_f64(rand::rng().random_range(0.5..=1.0))
    }
}

/// Is this worth trying again?
///
/// Config errors are not: the input will not change by retrying, and spinning
/// on one hides a typo behind a wall of identical log lines.
pub fn is_retryable(e: &IngesterError) -> bool {
    !matches!(e, IngesterError::Config(_))
}

/// Run `op` until it succeeds, hits an unretryable error, or exhausts
/// `policy.max_attempts`.
///
/// `what` and `chain_id` only shape the log line; they carry no behaviour.
pub async fn retrying<T, F, Fut>(
    policy: Policy,
    what: &str,
    chain_id: i64,
    mut op: F,
) -> Result<T, IngesterError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, IngesterError>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if !is_retryable(&e) => return Err(e),
            Err(e) => {
                attempt += 1;
                if attempt >= policy.max_attempts {
                    return Err(e);
                }
                let delay = policy.delay(attempt - 1);
                warn!(
                    chain_id,
                    what,
                    attempt,
                    max_attempts = policy.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    "{} failed, retrying: {}",
                    what,
                    e
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::RpcError;
    use std::cell::Cell;

    #[test]
    fn backoff_grows_and_stays_capped() {
        let p = Policy::LIVE_TICK;
        assert!(p.delay(0) <= p.base, "first retry is quick");
        for attempt in 0..40 {
            assert!(p.delay(attempt) <= p.max, "attempt {attempt} exceeded cap");
        }
    }

    /// A huge attempt count must not overflow the shift or the multiply.
    #[test]
    fn backoff_saturates_instead_of_overflowing() {
        assert!(Policy::LIVE_TICK.delay(u32::MAX) <= Policy::LIVE_TICK.max);
    }

    #[test]
    fn config_errors_are_not_retryable() {
        assert!(!is_retryable(&IngesterError::config("bad")));
        assert!(is_retryable(&IngesterError::Db("down".into())));
        assert!(is_retryable(&IngesterError::Rpc(RpcError::RateLimited)));
    }

    fn instant() -> Policy {
        Policy {
            max_attempts: 4,
            base: Duration::ZERO,
            max: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Cell::new(0);
        let got = retrying(instant(), "op", 1, || async {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(IngesterError::Db("flaky".into()))
            } else {
                Ok(calls.get())
            }
        })
        .await
        .expect("third attempt succeeds");
        assert_eq!(got, 3);
    }

    /// Giving up is the point: it releases the chain so a standby can try.
    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let calls = Cell::new(0);
        let err = retrying(instant(), "op", 1, || async {
            calls.set(calls.get() + 1);
            Err::<(), _>(IngesterError::Db("always".into()))
        })
        .await
        .expect_err("never succeeds");
        assert!(matches!(err, IngesterError::Db(_)));
        assert_eq!(calls.get(), 4, "exactly max_attempts calls");
    }

    /// A bad address will not fix itself, so it must not burn the budget.
    #[tokio::test]
    async fn does_not_retry_config_errors() {
        let calls = Cell::new(0);
        let _ = retrying(instant(), "op", 1, || async {
            calls.set(calls.get() + 1);
            Err::<(), _>(IngesterError::config("nope"))
        })
        .await;
        assert_eq!(calls.get(), 1);
    }
}
