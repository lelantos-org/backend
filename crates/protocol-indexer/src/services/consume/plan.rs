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
//! append-only tables, last-write-wins upserts for the rest — and the cursor
//! moves only after they all succeed, so a crash mid-apply replays the same
//! window and converges.
//!
//! # Shape
//!
//! One field per concern this crate projects, each owning its own rows and its
//! own writes: [`super::assets`], [`super::yields`], [`super::tree`],
//! [`super::deposits`], [`super::governance`]. This struct is only the
//! container and the order.
//!
//! # Ordering
//!
//! Rows are appended in event order and applied in the fixed group order of
//! [`CommitPlan::apply`] and of each sub-plan's own `apply`, which follows the
//! causal order the contract emits: a row is registered before its rate is set,
//! a venue is bound before its parameters change, and a deposit is escrowed
//! before it is flushed or canceled. Within a group the original order is
//! preserved, so repeated events for one key still resolve last-write-wins.

use super::assets::AssetPlan;
use super::deposits::DepositPlan;
use super::governance::GovernancePlan;
use super::tree::TreePlan;
use super::yields::YieldPlan;
use crate::domain::error::ProtocolIndexerError;
use database::DbPool;

#[derive(Debug, Default)]
pub struct CommitPlan {
    pub assets: AssetPlan,
    pub yields: YieldPlan,
    pub tree: TreePlan,
    pub deposits: DepositPlan,
    pub governance: GovernancePlan,
}

impl CommitPlan {
    /// Whether this window wrote anything into `tree_advances`.
    ///
    /// Drives the `tree_advances_hourly` rebuild; see `super::refresh`.
    pub fn touched_tree_advances(&self) -> bool {
        !self.tree.is_empty()
    }

    /// Issue every write, in causal group order.
    ///
    /// Each call is one pool checkout; the append-only tables get one statement
    /// for the whole group. Empty groups cost nothing — the batch functions
    /// return before checking out a connection.
    pub async fn apply(&self, pool: &DbPool) -> Result<(), ProtocolIndexerError> {
        self.assets.apply(pool).await?;
        self.yields.apply(pool).await?;
        self.tree.apply(pool).await?;
        self.deposits.apply(pool).await?;
        self.governance.apply(pool).await?;
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
        assert!(!CommitPlan::default().touched_tree_advances());
    }
}
