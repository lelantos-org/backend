//! What the flush worker refuses to batch, and for how long.
//!
//! `flushBatch` is all-or-nothing and `pop_pending` returns the oldest deposits
//! first, so a deposit the worker keeps declining occupies the batch window and
//! blocks every newer deposit on its chain. Two kinds of deposit do that, and
//! they are held apart here:
//!
//!   * **Quarantined** — it can never land, and its own fields prove it
//!     (`Verdict::Reject`), or it has failed the contract `max_attempts` times.
//!     Never batched again while this process lives.
//!   * **Deferred** — this relayer will not flush it *now*, and only the payer
//!     or a change in what a flush costs will alter that (`Verdict::Defer`,
//!     which is every way a fee leaf can fail to pay us). Held out of the window
//!     for a few ticks, with an exponential backoff, and reconsidered from
//!     scratch when the wait elapses.
//!
//! Deferral is what keeps an unpayable deposit from starving the chain. Without
//! it the worker re-judges the same head-of-window deposits every tick, reaches
//! the same verdict, and never reaches the payable deposits behind them.
//!
//! Neither is lost work for the payer: a stuck deposit is reclaimable with
//! `cancelDeposit` once `cancelDelay` has passed, and `flushBatch` is
//! permissionless, so a deposit this relayer declines can be flushed by one it
//! pays.
//!
//! Waits are counted in flush ticks rather than wall-clock, so they scale with
//! `flush_interval_s` and stay deterministic in tests. State is in-memory, since
//! the relayer keeps no tables of its own and `FlushPipeline::preflight`
//! re-derives every verdict on the next tick.

use crate::domain::error::AppError;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use tracing::{error, info};

/// Ticks a deposit waits after its first deferral. Short: the usual cause is a
/// fee quote the payer just missed, and gas moves.
const DEFER_BASE_TICKS: u64 = 2;

/// Ceiling on the doubling. At a 5s `flush_interval_s` this is a few minutes, so
/// a deposit that becomes payable — gas fell, or the oracle re-priced its asset —
/// waits minutes rather than for a restart, while a deposit nobody will ever pay
/// for costs one re-judgement every 64 ticks.
const DEFER_MAX_TICKS: u64 = 64;

/// One deposit's standing deferral.
struct Deferral {
    /// Tick number at which it re-enters the batch window.
    resume_at: u64,
    /// Consecutive deferrals, driving the backoff. Cleared once the deposit is
    /// judged flushable.
    strikes: u32,
}

/// How long a lapsed deferral is remembered after it comes due.
///
/// The strike count has to outlive the wait it caused, or a deposit re-judged and
/// re-deferred every time its wait elapses would restart at the shortest wait and
/// never actually back off. Entries are dropped once this much has passed with no
/// further deferral, which is also how a deposit that has since been flushed or
/// canceled leaves the map.
const DEFER_FORGET_TICKS: u64 = DEFER_MAX_TICKS;

#[derive(Default)]
struct State {
    /// Attributable failures per deposit, cleared wholesale on any success.
    attempts: HashMap<u64, u32>,
    quarantined: HashSet<u64>,
    /// Deposits held out of the batch window until a later tick. Pruned as they
    /// come due, so this holds only live deferrals.
    deferred: HashMap<u64, Deferral>,
    /// Flush attempts this process has started, the clock deferrals are measured
    /// against. Advanced by [`DepositFailures::begin_tick`], so a tick that bails
    /// out before looking at the mempool does not shorten anyone's wait.
    tick: u64,
    /// A batch failed and no single deposit could be blamed, so the next tick
    /// flushes one deposit at a time to make the next failure attributable.
    degraded: bool,
    /// Sticky: at least one deposit's digest has matched the chain, so
    /// `deposit_digest` is known to agree with this pool. Until then a mismatch is
    /// more likely a local bug than the deposit's fault, and quarantining on it
    /// would take out the whole mempool.
    digest_verified: bool,
}

pub struct DepositFailures {
    chain_id: i64,
    /// Attributable failures tolerated before a deposit is skipped. `0` disables
    /// quarantine; deposits are still dropped from a batch by pre-flight but never
    /// remembered.
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

    /// Start a flush attempt, advancing the clock deferrals are counted in.
    ///
    /// Called once per tick that reaches the mempool, immediately before
    /// [`Self::excluded_ids`].
    pub fn begin_tick(&self) {
        self.state.lock().tick += 1;
    }

    pub fn quarantined_ids(&self) -> Vec<u64> {
        self.state.lock().quarantined.iter().copied().collect()
    }

    /// Every deposit the next `pop_pending` should look past: quarantined for
    /// good, or deferred until a later tick.
    ///
    /// A deferral that has come due stops excluding its deposit immediately — the
    /// deposit is re-judged on this tick — but is remembered for
    /// `DEFER_FORGET_TICKS` so its backoff survives that re-judgement. Entries are
    /// pruned here rather than by a sweep.
    pub fn excluded_ids(&self) -> HashSet<u64> {
        let mut state = self.state.lock();
        let now = state.tick;
        state
            .deferred
            .retain(|_, d| now <= d.resume_at.saturating_add(DEFER_FORGET_TICKS));
        state
            .quarantined
            .iter()
            .copied()
            .chain(
                state
                    .deferred
                    .iter()
                    .filter(|(_, d)| d.resume_at > now)
                    .map(|(id, _)| *id),
            )
            .collect()
    }

    /// Hold `id` out of the batch window for a while: this relayer will not flush
    /// it, and only the payer or a change in what a flush costs will alter that.
    ///
    /// The wait doubles with each consecutive deferral, so a deposit nobody
    /// intends to pay for stops costing a judgement every tick, while one whose
    /// fee narrowly missed the quote is reconsidered within a few.
    pub fn defer(&self, id: u64, reason: &str) {
        let mut state = self.state.lock();
        let now = state.tick;
        let entry = state.deferred.entry(id).or_insert(Deferral {
            resume_at: now,
            strikes: 0,
        });
        entry.strikes = entry.strikes.saturating_add(1);
        // Doubling from the base on each consecutive deferral. The shift is
        // clamped well below `DEFER_MAX_TICKS`'s magnitude, so the ceiling is what
        // bounds the wait, not overflow.
        let shift = entry.strikes.saturating_sub(1).min(16);
        let wait = (DEFER_BASE_TICKS << shift).min(DEFER_MAX_TICKS);
        entry.resume_at = now + wait;
        info!(
            chain_id = self.chain_id,
            deposit_id = id,
            reason,
            strikes = entry.strikes,
            resume_at_tick = entry.resume_at,
            "deposit deferred; it will not be batched again for several ticks \
             (another relayer can flush it, and the payer can reclaim it with cancelDeposit)"
        );
    }

    /// The deposit is batchable again, so any standing deferral and its backoff
    /// are dropped: the next one starts from the shortest wait.
    pub fn note_flushable(&self, id: u64) {
        self.state.lock().deferred.remove(&id);
    }

    /// Deferrals still held, lapsed ones included. Test-only: the map is pruned
    /// lazily, and nothing else should depend on its size.
    #[cfg(test)]
    fn deferred_len(&self) -> usize {
        self.state.lock().deferred.len()
    }

    /// Stop batching this deposit. `reason` is a fixed string rather than node
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

    /// A flush landed, so the chain is healthy and every attempt counted so far was
    /// more likely batch-level noise than a bad deposit. Existing quarantines stand,
    /// since those were deterministic judgements rather than counted ones.
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
        // With more than one deposit in the batch there is no way to tell which one
        // the contract refused, and charging all of them would quarantine the
        // majority. The batch shrinks instead, so the next failure names its
        // deposit.
        let [id] = ids else {
            self.degrade(&mut state, ids.len(), cause);
            return;
        };
        let attempts = state.attempts.entry(*id).or_insert(0);
        *attempts += 1;
        let attempts = *attempts;
        // `max_attempts == 0` disables quarantine, so the count still runs and
        // logs but never reaches a verdict.
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

    /// The one place quarantine happens, so `max_attempts == 0` disables every path
    /// into it, counted and deterministic alike, and every quarantine is logged
    /// once.
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
/// otherwise quarantine the entire mempool within a few ticks. `SubmitUnknown`
/// parks the mirror on its own and `MirrorDesynced` stops the worker.
///
/// The remaining classes can still be batch-level rather than per-deposit, since
/// `StaleOldRoot` and `TreeUpdateRejected` both surface as a revert. Charging
/// them to the head deposit is acceptable: those states stop the chain flushing
/// at all, and [`DepositFailures::note_success`] clears the counts once anything
/// lands.
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
        // The two earlier charges are cleared, so this one starts from zero.
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

    /// Degradation is not quarantine, so it applies even with quarantine disabled:
    /// batching one at a time is how a bad deposit is isolated.
    #[test]
    fn zero_max_attempts_still_shrinks_the_batch() {
        let f = DepositFailures::new(1, 0);
        f.note_failure(&[1, 2], &reverted());
        assert_eq!(f.batch_limit(8), 1);
    }

    /// Ticks the worker would have run, so a test can wait out a backoff without
    /// sleeping.
    fn run_ticks(f: &DepositFailures, n: u64) {
        for _ in 0..n {
            f.begin_tick();
        }
    }

    /// The bug this exists for: without deferral the same underpaid deposit is
    /// re-judged every tick and fills the batch window, and the payable deposits
    /// behind it never get in.
    #[test]
    fn a_deferred_deposit_leaves_the_batch_window() {
        let f = DepositFailures::new(1, 1);
        f.begin_tick();
        f.defer(7, "fee note does not cover the flush");
        assert!(f.excluded_ids().contains(&7));
        assert!(f.quarantined_ids().is_empty(), "deferral is not quarantine");
    }

    #[test]
    fn a_deferral_expires_and_the_deposit_is_judged_again() {
        let f = DepositFailures::new(1, 1);
        f.begin_tick();
        f.defer(7, "fee note is not addressed to this relayer");
        run_ticks(&f, DEFER_BASE_TICKS - 1);
        assert!(f.excluded_ids().contains(&7), "still inside the first wait");
        run_ticks(&f, 1);
        assert!(f.excluded_ids().is_empty(), "the wait elapsed");
    }

    /// A deposit nobody intends to pay for must stop costing a judgement every
    /// tick, so consecutive deferrals wait longer each time.
    #[test]
    fn consecutive_deferrals_back_off() {
        let f = DepositFailures::new(1, 1);
        f.begin_tick();
        f.defer(7, "underpaid");
        run_ticks(&f, DEFER_BASE_TICKS);
        assert!(f.excluded_ids().is_empty());

        f.defer(7, "underpaid again");
        run_ticks(&f, DEFER_BASE_TICKS);
        assert!(
            f.excluded_ids().contains(&7),
            "the second wait must outlast the first"
        );
        run_ticks(&f, DEFER_BASE_TICKS);
        assert!(f.excluded_ids().is_empty());
    }

    /// Otherwise a deposit that was underpaid for a while would keep a long
    /// backoff after its fee became sufficient, and a later shortfall would wait
    /// minutes to be reconsidered.
    #[test]
    fn becoming_flushable_clears_the_backoff() {
        let f = DepositFailures::new(1, 1);
        f.begin_tick();
        for _ in 0..5 {
            f.defer(7, "underpaid");
        }
        f.note_flushable(7);
        assert!(f.excluded_ids().is_empty());

        // Back to the shortest wait rather than resuming the doubling.
        f.defer(7, "underpaid");
        run_ticks(&f, DEFER_BASE_TICKS);
        assert!(f.excluded_ids().is_empty());
    }

    /// The wait is bounded, so a deposit that becomes payable — gas fell, or its
    /// asset re-priced — is picked up within minutes rather than at the next
    /// restart.
    #[test]
    fn the_backoff_is_capped() {
        let f = DepositFailures::new(1, 1);
        f.begin_tick();
        for _ in 0..64 {
            f.defer(7, "underpaid");
        }
        run_ticks(&f, DEFER_MAX_TICKS);
        assert!(f.excluded_ids().is_empty());
    }

    /// A deposit that stops being pending — flushed by another relayer, or
    /// canceled — is never deferred again, so its entry must not sit in the map
    /// for the life of the process.
    #[test]
    fn a_deferral_nobody_renews_is_forgotten() {
        let f = DepositFailures::new(1, 1);
        f.begin_tick();
        f.defer(7, "underpaid");
        run_ticks(&f, DEFER_BASE_TICKS + DEFER_FORGET_TICKS + 1);
        assert!(f.excluded_ids().is_empty());
        assert!(f.deferred_len() == 0, "the lapsed entry must be pruned");
    }

    /// Both exclusions feed one query, and they are independent: a quarantine is
    /// permanent where a deferral times out.
    #[test]
    fn exclusions_cover_quarantined_and_deferred_alike() {
        let f = DepositFailures::new(1, 1);
        f.begin_tick();
        f.quarantine(1, "digest mismatch");
        f.defer(2, "underpaid");
        assert_eq!(f.excluded_ids(), HashSet::from([1, 2]));
        run_ticks(&f, DEFER_MAX_TICKS);
        assert_eq!(
            f.excluded_ids(),
            HashSet::from([1]),
            "the quarantine outlives the deferral"
        );
    }

    #[test]
    fn quarantining_the_same_deposit_twice_is_idempotent() {
        let f = DepositFailures::new(1, 1);
        f.quarantine(3, "digest mismatch");
        f.quarantine(3, "digest mismatch");
        assert_eq!(f.quarantined_ids(), vec![3]);
    }
}
