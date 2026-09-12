//! One tick's writes, collected before any of them is issued.
//!
//! The consume loop used to write as it decoded: one statement, and one pool
//! checkout, per event. A 500-event batch was therefore 500 sequential round
//! trips against a pool of eight connections, and the cursor advanced in a
//! statement of its own afterwards.
//!
//! Now the whole window is decoded into this struct and written in one pass of
//! batched calls. It mirrors `fmd_indexer`'s `CommitPlan`, including its
//! recovery story: there is no enclosing transaction, and there deliberately is
//! not one. Every write is idempotent — `ON CONFLICT DO NOTHING` for the
//! append-only tables — and the cursor moves only after they all succeed, so a
//! crash mid-apply replays the same window and converges.
//!
//! Both tables here are append-only and independent, so unlike the protocol
//! half's plan there is no causal group order to preserve: nothing written here
//! is an `UPDATE` keyed on a row another group inserts.

use crate::domain::error::ExplorerIndexerError;
use crate::repositories::{
    asset_flows::{self, NewAssetFlow},
    yield_fee_events::{self, NewYieldFeeEvent},
};
use database::DbPool;

#[derive(Debug, Default)]
pub struct CommitPlan {
    pub flows: Vec<NewAssetFlow>,
    pub yield_fees: Vec<NewYieldFeeEvent>,
}

impl CommitPlan {
    /// Whether this window wrote anything into `asset_flows`.
    ///
    /// Drives the `asset_flows_hourly` / `asset_locked` rebuild; see
    /// `super::refresh`.
    pub fn touched_flows(&self) -> bool {
        !self.flows.is_empty()
    }

    /// Issue every write.
    ///
    /// Each call is one pool checkout, one statement for the whole group. Empty
    /// groups cost nothing — the batch functions return before checking out a
    /// connection.
    pub async fn apply(&self, pool: &DbPool) -> Result<(), ExplorerIndexerError> {
        asset_flows::insert_batch(pool, &self.flows).await?;
        yield_fee_events::insert_batch(pool, &self.yield_fees).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refresh gate keys off this, so an empty window must not mark the view
    /// dirty and buy a full aggregation for nothing.
    #[test]
    fn an_empty_plan_touches_no_view() {
        assert!(!CommitPlan::default().touched_flows());
    }
}
