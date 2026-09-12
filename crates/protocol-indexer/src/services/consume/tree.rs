//! Tree advances: `RootAdvanced` into `tree_advances`.
//!
//! Read by explorer-webserver through `tree_advances_hourly`, and by the relayer
//! as the log of what it has already submitted — which is why this projection
//! lives here rather than behind a service allowed to lag.
//!
//! Pure up to [`TreePlan::apply`] — the push method builds rows and touches no
//! database.

use crate::domain::error::ProtocolIndexerError;
use crate::repositories::tree_advances::{self, TreeAdvanceRow};
use alloy::primitives::B256;
use database::{DbPool, RawEventRow};

/// One window's writes to `tree_advances`.
#[derive(Debug, Default)]
pub struct TreePlan {
    pub advances: Vec<TreeAdvanceRow>,
}

impl TreePlan {
    pub fn push_advance(
        &mut self,
        chain_id: i64,
        row: &RawEventRow,
        start_index: u64,
        inserted: u64,
        old_root: B256,
        new_root: B256,
    ) {
        self.advances.push(TreeAdvanceRow {
            chain_id,
            block_number: row.block_number,
            log_index: row.log_index,
            start_index: start_index as i64,
            inserted: inserted as i32,
            old_root: old_root.0.to_vec(),
            new_root: new_root.0.to_vec(),
            tx_hash: row.tx_hash.clone(),
            block_ts: row.block_ts,
        });
    }

    /// Whether this window carries no advance at all.
    ///
    /// Inverted by `CommitPlan::touched_tree_advances` to drive the
    /// `tree_advances_hourly` rebuild; see `super::refresh`. The view is read by
    /// explorer-webserver but refreshed here, because a materialized view has to
    /// be rebuilt by whoever writes its base table — nothing else knows when the
    /// rows changed.
    pub fn is_empty(&self) -> bool {
        self.advances.is_empty()
    }

    pub async fn apply(&self, pool: &DbPool) -> Result<(), ProtocolIndexerError> {
        tree_advances::insert_batch(pool, &self.advances).await?;
        Ok(())
    }
}
