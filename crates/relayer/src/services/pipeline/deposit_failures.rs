//! Bounded-attempt tracking for the flush worker.
//!
//! `flushBatch` is all-or-nothing and `pop_pending` always returns the oldest
//! deposits first, so one deposit that can never land blocks every newer
//! deposit on its chain — forever, at the cost of a `tree_update_batch`
//! Groth16 per tick. This gives the pipeline a way to give up on one deposit
//! instead of on the chain.
//!
//! Giving up is safe because a stuck deposit is not lost funds: the payer can
//! reclaim it with `cancelDeposit` once `cancelDelay` has passed.
//!
//! State is deliberately in-memory — the relayer keeps no tables of its own,
//! and the deterministic rejection classes are re-derived on the next tick by
//! `FlushPipeline::preflight` anyway.

use crate::domain::error::AppError;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use tracing::{error, info};

#[derive(Default)]
struct State {
    /// Attributable failures per deposit, cleared wholesale on any success.
    attempts: HashMap<u64, u32>,
    quarantined: HashSet<u64>,
    /// A batch failed and no single deposit could be blamed, so the next tick
    /// flushes one deposit at a time to make the next failure attributable.
    degraded: bool,
    /// Sticky: some deposit's digest has matched the chain at least once, so
    /// `deposit_digest` is known to agree with this pool. Until then a
    /// mismatch is more likely our bug than the deposit's, and quarantining
    /// on it would take out the whole mempool.
    digest_verified: bool,
}

pub struct DepositFailures {
    chain_id: i64,
    /// Attributable failures tolerated before a deposit is skipped. `0`
    /// disables quarantine entirely; deposits are still dropped from a batch
    /// by pre-flight, just never remembered.
    max_attempts: u32,
    state: Mutex<State>,
}

impl DepositFailures {
    pub fn new(chain_id: i64, max_attempts: u32) -> Self {
        Self {
            chain_id,
            max_attempts,
            state: Mutex::new(State::default()),
        }
    }

    /// How many deposits the next tick should batch, given `max_n`.
    pub fn batch_limit(&self, max_n: usize) -> usize {
        if self.state.lock().degraded { 1 } else { max_n }
    }

    pub fn quarantined_ids(&self) -> Vec<u64> {
        self.state.lock().quarantined.iter().copied().collect()
    }

    /// Stop batching this deposit. `reason` is a fixed string, never node
    /// text, so it is safe in a log line and stable enough to alert on.
    pub fn quarantine(&self, id: u64, reason: &str) {
        let mut state = self.state.lock();
        self.quarantine_locked(&mut state, id, reason);
    }

    /// Record that this pool's digests are derivable — see
    /// [`State::digest_verified`].
    pub fn note_digest_verified(&self) {
        self.state.lock().digest_verified = true;
    }

    pub fn digest_verified(&self) -> bool {
        self.state.lock().digest_verified
    }

    /// A flush landed: the chain is healthy, so every attempt counted so far
    /// was more likely batch-level noise than a bad deposit. Quarantines
    /// stand — those were deterministic judgements, not counted ones.
    pub fn note_success(&self) {
        let mut state = self.state.lock();
        state.attempts.clear();
        if std::mem::take(&mut state.degraded) {
            info!(
                chain_id = self.chain_id,
                "flush recovered; resuming full-size batches"
            );
        }
    }

    /// Charge a failed batch to the deposits in it, if the failure was theirs
    /// to own.
    pub fn note_failure(&self, ids: &[u64], cause: &AppError) {
        if !is_batch_attributable(cause) {
            return;
        }
        let mut state = self.state.lock();
        // With more than one deposit in the batch there is no way to tell
        // which one the contract refused, and charging all of them would
        // quarantine the innocent majority. Shrink the batch instead; the
        // next failure names its deposit.
        let [id] = ids else {
            self.degrade(&mut state, ids.len(), cause);
            return;
        };
        let attempts = state.attempts.entry(*id).or_insert(0);
        *attempts += 1;
        let attempts = *attempts;
        // `max_attempts == 0` disables quarantine, so the count still runs
        // and still logs — it just never reaches a verdict.
        if self.max_attempts > 0 && attempts >= self.max_attempts {
            self.quarantine_locked(&mut state, *id, "exhausted flush attempts");
        } else {
            info!(
                chain_id = self.chain_id,
                deposit_id = *id,
                attempts,
                max_attempts = self.max_attempts,
                error = %cause,
                "flush failed for a single deposit"
            );
        }
    }

    /// Flush one deposit at a time until a failure can be pinned on one.
    fn degrade(&self, state: &mut State, batched: usize, cause: &AppError) {
        if std::mem::replace(&mut state.degraded, true) {
            return;
        }
        info!(
            chain_id = self.chain_id,
            n = batched,
            error = %cause,
            "flush batch failed; flushing one deposit at a time to isolate it"
        );
    }

    /// The single place quarantine happens, so `max_attempts == 0` disables
    /// every path into it — counted and deterministic alike — and every
    /// quarantine is logged exactly once.
    fn quarantine_locked(&self, state: &mut State, id: u64, reason: &str) {
        if self.max_attempts == 0 || !state.quarantined.insert(id) {
            return;
        }
        error!(
            chain_id = self.chain_id,
            deposit_id = id,
            reason,
            "deposit quarantined; it will not be batched again until the relayer restarts \
             (the payer can reclaim it with cancelDeposit)"
        );
    }
}

/// Whether a failure says anything about the deposits that were in the batch.
///
/// Infrastructure faults must not count: an RPC outage or a busy prover would
/// otherwise quarantine the entire mempool a few ticks in. `SubmitUnknown`
/// parks the mirror on its own, and `MirrorDesynced` stops the worker.
///
/// The remaining classes can still be batch-level rather than per-deposit —
/// `StaleOldRoot` and `TreeUpdateRejected` both surface as a revert. Charging
/// them to the head deposit is tolerable: those states stop the chain flushing
/// at all, and [`DepositFailures::note_success`] clears the counts as soon as
/// anything lands again.
fn is_batch_attributable(cause: &AppError) -> bool {
    matches!(
        cause,
        AppError::Reverted(_) | AppError::ContractRejected { .. } | AppError::Prover(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reverted() -> AppError {
        AppError::Reverted("tx 0xdead reverted".into())
    }

    #[test]
    fn infrastructure_failures_never_charge_a_deposit() {
        let f = DepositFailures::new(1, 1);
        for e in [
            AppError::Rpc("node down".into()),
            AppError::Db("pool exhausted".into()),
            AppError::ProverBusy,
            AppError::SubmitUnknown("no receipt".into()),
            AppError::MirrorDesynced("parked".into()),
            AppError::Internal("boom".into()),
        ] {
            f.note_failure(&[7], &e);
        }
        assert!(f.quarantined_ids().is_empty());
        assert_eq!(f.batch_limit(8), 8, "infrastructure must not degrade");
    }

    #[test]
    fn a_multi_deposit_failure_shrinks_the_batch_instead_of_blaming_anyone() {
        let f = DepositFailures::new(1, 1);
        f.note_failure(&[1, 2, 3], &reverted());
        assert!(f.quarantined_ids().is_empty());
        assert_eq!(f.batch_limit(8), 1);
    }

    #[test]
    fn a_single_deposit_failure_is_charged_and_quarantined_at_the_threshold() {
        let f = DepositFailures::new(1, 3);
        f.note_failure(&[9], &reverted());
        f.note_failure(&[9], &reverted());
        assert!(f.quarantined_ids().is_empty(), "not yet at the threshold");
        f.note_failure(&[9], &reverted());
        assert_eq!(f.quarantined_ids(), vec![9]);
    }

    #[test]
    fn a_success_clears_the_counts_and_restores_full_batches() {
        let f = DepositFailures::new(1, 3);
        f.note_failure(&[1, 2], &reverted());
        f.note_failure(&[1], &reverted());
        f.note_failure(&[1], &reverted());
        f.note_success();
        assert_eq!(f.batch_limit(8), 8);
        // The two earlier charges are gone, so this one starts from zero.
        f.note_failure(&[1], &reverted());
        assert!(f.quarantined_ids().is_empty());
    }

    /// The counted path and the deterministic path both go through the knob.
    #[test]
    fn zero_max_attempts_disables_quarantine_entirely() {
        let f = DepositFailures::new(1, 0);
        for _ in 0..100 {
            f.note_failure(&[4], &reverted());
        }
        f.quarantine(5, "digest mismatch");
        assert!(f.quarantined_ids().is_empty());
    }

    /// Degradation is not quarantine, so it still applies with the knob off:
    /// batching one at a time is how a poison deposit is isolated at all.
    #[test]
    fn zero_max_attempts_still_shrinks_the_batch() {
        let f = DepositFailures::new(1, 0);
        f.note_failure(&[1, 2], &reverted());
        assert_eq!(f.batch_limit(8), 1);
    }

    #[test]
    fn quarantining_the_same_deposit_twice_is_idempotent() {
        let f = DepositFailures::new(1, 1);
        f.quarantine(3, "digest mismatch");
        f.quarantine(3, "digest mismatch");
        assert_eq!(f.quarantined_ids(), vec![3]);
    }
}
