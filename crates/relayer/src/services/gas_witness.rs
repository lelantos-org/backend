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

use crate::domain::dto::SpendKind;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Process-wide, one per chain. Lock-free: quoting must not queue behind
/// submissions.
pub struct GasWitness {
    observed: [AtomicU64; EntryPoint::COUNT],
}

impl GasWitness {
    pub fn new() -> Self {
        Self {
            observed: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Record a confirmed submission's gas.
    pub fn observe(&self, entry: EntryPoint, gas_used: u64) {
        self.observed[entry.index()].store(gas_used, Ordering::Relaxed);
    }

    /// Best estimate for the next call, floored at the seed so one unusually
    /// cheap tx (warm storage, no token transfer) cannot under-quote the next.
    pub fn gas_for(&self, entry: EntryPoint) -> u64 {
        self.observed[entry.index()]
            .load(Ordering::Relaxed)
            .max(entry.seed())
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
