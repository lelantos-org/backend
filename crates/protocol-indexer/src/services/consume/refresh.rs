//! When to rebuild the protocol indexer's materialized views.
//!
//! `tree_advances_hourly` is a whole-table aggregate over `tree_advances`. The
//! decision of *when* to rebuild lives in [`shared::refresh`]; this module names
//! the view and runs it.
//!
//! The view is global, not per-chain, so the gate is too: N chains ticking a
//! change into the same view is one refresh, not N.

use crate::repositories::tree_advances;
use database::DbPool;
use shared::refresh::{ViewState, report};
use tokio::sync::Mutex;

/// A group of views rebuilt together because they derive from the same rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// `tree_advances_hourly`, from `RootAdvanced`.
    TreeAdvances,
}

#[derive(Debug)]
struct State {
    tree_advances: ViewState,
}

/// Decides which views to rebuild, and when.
///
/// Shared across every chain's tick, so one `ConsumeServiceImpl` holds one of
/// these. The lock is held across the refresh itself, which is deliberate: two
/// overlapping `REFRESH MATERIALIZED VIEW CONCURRENTLY` statements on one view
/// would queue on Postgres' own lock anyway, and serialising here keeps the
/// dirty flags honest about what has actually been rebuilt.
#[derive(Debug)]
pub struct RefreshGate {
    state: Mutex<State>,
}

impl RefreshGate {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                tree_advances: ViewState::new(),
            }),
        }
    }

    /// Note that `view`'s source rows changed. Cheap; does no IO.
    pub async fn mark(&self, view: View) {
        let mut st = self.state.lock().await;
        match view {
            View::TreeAdvances => st.tree_advances.mark(),
        }
    }

    /// Rebuild whichever views are dirty and due.
    ///
    /// `caught_up` is false while the tick that just ran came back saturated,
    /// which is the signal that more rows are already queued behind it.
    pub async fn flush(&self, pool: &DbPool, caught_up: bool) {
        let mut st = self.state.lock().await;

        if st.tree_advances.due(caught_up) {
            let ok = report(
                "tree_advances_hourly",
                tree_advances::refresh_hourly_mv(pool).await,
            );
            st.tree_advances.record(ok);
        }
    }
}

impl Default for RefreshGate {
    fn default() -> Self {
        Self::new()
    }
}
