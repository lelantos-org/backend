//! When to rebuild the explorer's materialized views.
//!
//! `asset_flows_hourly` and `asset_locked` are whole-table aggregates over
//! `asset_flows`. The decision of *when* to rebuild lives in
//! [`shared::refresh`]; this module names the views and runs them.
//!
//! The views are global, not per-chain, so the gate is too: N chains ticking a
//! change into the same view is one refresh, not N.

use crate::repositories::asset_flows;
use database::DbPool;
use shared::refresh::{ViewState, report};
use tokio::sync::Mutex;

/// A group of views rebuilt together because they derive from the same rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// `asset_flows_hourly` and `asset_locked`, both from `asset_flows`.
    AssetFlows,
}

#[derive(Debug)]
struct State {
    asset_flows: ViewState,
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
                asset_flows: ViewState::new(),
            }),
        }
    }

    /// Note that `view`'s source rows changed. Cheap; does no IO.
    pub async fn mark(&self, view: View) {
        let mut st = self.state.lock().await;
        match view {
            View::AssetFlows => st.asset_flows.mark(),
        }
    }

    /// Rebuild whichever views are dirty and due.
    ///
    /// `caught_up` is false while the tick that just ran came back saturated,
    /// which is the signal that more rows are already queued behind it.
    pub async fn flush(&self, pool: &DbPool, caught_up: bool) {
        let mut st = self.state.lock().await;

        if st.asset_flows.due(caught_up) {
            // Both derive from `asset_flows`, and both are attempted so a failure
            // on one does not leave the other stale. The group is dirty until
            // both succeed.
            let hourly = report(
                "asset_flows_hourly",
                asset_flows::refresh_hourly_mv(pool).await,
            );
            let locked = report("asset_locked", asset_flows::refresh_locked_mv(pool).await);
            st.asset_flows.record(hourly && locked);
        }
    }
}

impl Default for RefreshGate {
    fn default() -> Self {
        Self::new()
    }
}
