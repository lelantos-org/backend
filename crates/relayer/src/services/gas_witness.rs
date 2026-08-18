//! Per-entry-point gas, learned from the relayer's own receipts.
//!
//! Quoting a fee used to mean building real calldata — which meant a full
//! `tree_update_batch` Groth16 — purely so `eth_estimateGas` had something the
//! contract's verifier would accept. That put a multi-second, single-threaded
//! proof on an unauthenticated request path, ahead of every real submission
//! waiting on the same prover.
//!
//! Gas for these entry points is dominated by fixed costs (two pairing checks,
//! one tree advance, one token transfer), so the last observed value is a good
//! predictor of the next one, and `fee_markup_bps` absorbs the jitter. Each
//! successful submission feeds its `gas_used` back here.
//!
//! Note this is a *fee* quote, not a gas limit: submissions still take their
//! limit from alloy's own per-tx estimate, so a stale value here only shifts a
//! little cost between relayer and user.
//!
//! Swaps break the "last value predicts the next" assumption: gas there scales
//! with route length and adapter, so a single-hop observation would under-quote
//! the multi-hop that follows. Every entry point therefore quotes the *high
//! water mark* of a bounded window of recent observations rather than the last
//! one, which tracks the expensive shape while still decaying once the window
//! has rolled past it.

use crate::domain::dto::SpendKind;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Discriminants double as slot indices into [`GasWitness::observed`], so they
/// must stay `0..COUNT` and contiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum EntryPoint {
    Transfer = 0,
    Withdraw = 1,
    WithdrawNative = 2,
    Swap = 3,
}

impl EntryPoint {
    pub const ALL: [EntryPoint; 4] = [
        EntryPoint::Transfer,
        EntryPoint::Withdraw,
        EntryPoint::WithdrawNative,
        EntryPoint::Swap,
    ];
    const COUNT: usize = Self::ALL.len();

    fn index(self) -> usize {
        self as usize
    }

    /// Cold-start value, used until this entry point has been submitted once.
    /// Deliberately on the high side: over-quoting costs the user a little,
    /// under-quoting costs the relayer.
    fn seed(self) -> u64 {
        match self {
            EntryPoint::Transfer => 500_000,
            EntryPoint::Withdraw => 560_000,
            EntryPoint::WithdrawNative => 600_000,
            EntryPoint::Swap => 950_000,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EntryPoint::Transfer => "transfer",
            EntryPoint::Withdraw => "withdraw",
            EntryPoint::WithdrawNative => "withdrawNative",
            EntryPoint::Swap => "swap",
        }
    }
}

impl From<SpendKind> for EntryPoint {
    fn from(k: SpendKind) -> Self {
        match k {
            SpendKind::Transfer => EntryPoint::Transfer,
            SpendKind::Withdraw => EntryPoint::Withdraw,
            SpendKind::WithdrawNative => EntryPoint::WithdrawNative,
        }
    }
}

/// How many recent observations an entry point quotes over. Small enough that
/// a genuinely cheaper deployment is reflected within a handful of
/// submissions, large enough that one cheap swap does not under-quote the
/// expensive route that follows it.
const WINDOW: usize = 8;

/// Process-wide, one per chain. Lock-free: quoting must not queue behind
/// submissions.
pub struct GasWitness {
    windows: [Window; EntryPoint::COUNT],
}

/// A fixed ring of recent observations. `0` means "no observation yet", which
/// is also the value a fresh slot holds, so an unfilled ring simply
/// contributes nothing to the maximum.
struct Window {
    slots: [AtomicU64; WINDOW],
    next: AtomicUsize,
}

impl Window {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| AtomicU64::new(0)),
            next: AtomicUsize::new(0),
        }
    }

    fn record(&self, gas_used: u64) {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % WINDOW;
        self.slots[i].store(gas_used, Ordering::Relaxed);
    }

    fn high_water(&self) -> u64 {
        self.slots
            .iter()
            .fold(0, |max, s| max.max(s.load(Ordering::Relaxed)))
    }
}

impl GasWitness {
    pub fn new() -> Self {
        Self {
            windows: std::array::from_fn(|_| Window::new()),
        }
    }

    /// Record a confirmed submission's gas.
    pub fn observe(&self, entry: EntryPoint, gas_used: u64) {
        self.windows[entry.index()].record(gas_used);
    }

    /// Best estimate for the next call: the most expensive of the last
    /// [`WINDOW`] submissions, floored at the seed so one unusually cheap tx
    /// (warm storage, no token transfer) cannot under-quote the next.
    pub fn gas_for(&self, entry: EntryPoint) -> u64 {
        self.windows[entry.index()].high_water().max(entry.seed())
    }
}

impl Default for GasWitness {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_start_falls_back_to_the_seed() {
        let w = GasWitness::new();
        assert_eq!(w.gas_for(EntryPoint::Transfer), EntryPoint::Transfer.seed());
    }

    #[test]
    fn observation_above_the_seed_wins() {
        let w = GasWitness::new();
        w.observe(EntryPoint::Withdraw, 700_000);
        assert_eq!(w.gas_for(EntryPoint::Withdraw), 700_000);
    }

    #[test]
    fn observation_below_the_seed_is_floored() {
        let w = GasWitness::new();
        w.observe(EntryPoint::Swap, 1);
        assert_eq!(w.gas_for(EntryPoint::Swap), EntryPoint::Swap.seed());
    }

    /// The swap case: a cheap single-hop right after an expensive multi-hop
    /// must not drag the quote down to the cheap one.
    #[test]
    fn a_cheap_observation_does_not_undo_an_expensive_one() {
        let w = GasWitness::new();
        w.observe(EntryPoint::Swap, 2_000_000);
        w.observe(EntryPoint::Swap, 1_000_000);
        assert_eq!(w.gas_for(EntryPoint::Swap), 2_000_000);
    }

    /// But the window is bounded, so a one-off spike does eventually roll off
    /// rather than over-quoting forever.
    #[test]
    fn an_old_spike_rolls_out_of_the_window() {
        let w = GasWitness::new();
        w.observe(EntryPoint::Swap, 5_000_000);
        for _ in 0..WINDOW {
            w.observe(EntryPoint::Swap, 1_000_000);
        }
        assert_eq!(w.gas_for(EntryPoint::Swap), 1_000_000);
    }

    #[test]
    fn entry_points_do_not_share_a_slot() {
        let w = GasWitness::new();
        w.observe(EntryPoint::Transfer, 1_000_000);
        assert_eq!(w.gas_for(EntryPoint::Withdraw), EntryPoint::Withdraw.seed());
    }

    /// `index` is a raw discriminant cast, so an out-of-range or duplicated
    /// value would index the wrong slot — or panic — rather than fail to
    /// compile.
    #[test]
    fn discriminants_are_valid_slot_indices() {
        let indices: Vec<usize> = EntryPoint::ALL.iter().map(|e| e.index()).collect();
        assert_eq!(indices, (0..EntryPoint::COUNT).collect::<Vec<_>>());
    }
}
