//! Yield-index poller.
//!
//! Refreshes `asset_yield`'s polled half from the chain. Everything else in this
//! crate is log-driven; this is not, because the quantity every conversion needs
//! — `gross / supply` — moves with the venue's own accounting on every block and
//! emits nothing when it does.
//!
//! Polling is sound rather than merely convenient: at 5% APY the index moves on
//! the order of 1e-7 per minute, far below the tolerances any consumer applies
//! to it. The one discontinuity is a venue loss, after which a reader holding
//! the previous value briefly over-values a unit until the next pass.

mod tick;

pub use tick::{YieldStateCtx, tick_chain};

use crate::adapters::masp::DynMaspYieldReader;
use async_trait::async_trait;
use database::DbPool;
use shared::tick::TickProgress;
use std::collections::HashMap;
use std::sync::Arc;

/// Owns its context rather than rebuilding one per tick.
///
/// The context carries the last-written state per asset, which is what lets an
/// unchanged row skip its `UPDATE`. Rebuilt per tick that cache would be empty
/// every round and the skip would never fire.
pub struct YieldStateServiceImpl {
    ctx: YieldStateCtx,
}

impl YieldStateServiceImpl {
    pub fn new(pool: DbPool, readers: Arc<HashMap<i64, DynMaspYieldReader>>) -> Self {
        Self {
            ctx: YieldStateCtx::new(pool, readers),
        }
    }
}

#[async_trait]
impl shared::tick::TickService for YieldStateServiceImpl {
    fn name(&self) -> &'static str {
        tick::NAME
    }

    /// The chains this crate indexes, not only those with yield assets: an
    /// asset can be bound at any time, and a chain with none simply ticks idle.
    async fn list_chain_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.ctx.readers.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    async fn tick_chain(&self, chain_id: i64, batch: i64) -> anyhow::Result<TickProgress> {
        tick::tick_chain(&self.ctx, chain_id, batch)
            .await
            .map_err(Into::into)
    }
}
