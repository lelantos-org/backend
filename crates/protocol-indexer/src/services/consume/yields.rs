//! Yield bindings: `YieldAssetAdded`, `YieldParamsSet` and `HaltedSet` into the
//! event-sourced half of `asset_yield`.
//!
//! The polled half — the index the venue moves on every block — is written by
//! `services::yield_state`, disjointly by column.
//!
//! Pure up to [`YieldPlan::apply`] — the push methods build rows and touch no
//! database.

use crate::domain::error::ProtocolIndexerError;
use crate::repositories::asset_yield::{self, SetHalted, SetParams, UpsertBinding};
use alloy::primitives::Address;
use database::DbPool;

/// One window's writes to `asset_yield`'s event-sourced columns.
#[derive(Debug, Default)]
pub struct YieldPlan {
    pub bindings: Vec<UpsertBinding>,
    pub params: Vec<SetParams>,
    pub halted: Vec<SetHalted>,
}

impl YieldPlan {
    /// Bind a venue, replacing any earlier binding for the same asset.
    ///
    /// Emitted once per asset and never reversed, so the row this creates is
    /// what marks the asset yield-bearing.
    ///
    /// The plan holds at most one binding per `(chain_id, asset_id_u64)`, so a
    /// window carrying two `YieldAssetAdded` events for one asset writes the
    /// later one — what writing them in sequence would have left behind.
    pub fn push_binding(
        &mut self,
        chain_id: i64,
        asset_id: u64,
        venue: Address,
        buffer_bps: u16,
        perf_bps: u16,
    ) {
        let row = UpsertBinding {
            chain_id,
            asset_id_u64: asset_id as i64,
            venue: venue.as_slice().to_vec(),
            // `bufferBps` is bounded by `BPS_DENOMINATOR` and `perfBps` by
            // `MAX_FEE_BPS`, so neither can lose a valid value as `SMALLINT`.
            buffer_bps: buffer_bps as i16,
            perf_bps: perf_bps as i16,
        };
        self.bindings
            .retain(|b| (b.chain_id, b.asset_id_u64) != (row.chain_id, row.asset_id_u64));
        self.bindings.push(row);
    }

    pub fn push_params(&mut self, chain_id: i64, asset_id: u64, buffer_bps: u16, perf_bps: u16) {
        self.params.push(SetParams {
            chain_id,
            asset_id_u64: asset_id as i64,
            buffer_bps: buffer_bps as i16,
            perf_bps: perf_bps as i16,
        });
    }

    pub fn push_halted(&mut self, chain_id: i64, asset_id: u64, halted: bool) {
        self.halted.push(SetHalted {
            chain_id,
            asset_id_u64: asset_id as i64,
            halted,
        });
    }

    pub async fn apply(&self, pool: &DbPool) -> Result<(), ProtocolIndexerError> {
        // Binding before parameters and halts: both are plain `UPDATE`s that
        // match nothing until the venue row exists.
        asset_yield::upsert_binding_batch(pool, &self.bindings).await?;
        asset_yield::set_params_batch(pool, &self.params).await?;
        asset_yield::set_halted_batch(pool, &self.halted).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(bindings: &[(i64, u64, i16)]) -> YieldPlan {
        let mut plan = YieldPlan::default();
        for (chain_id, asset_id, perf_bps) in bindings {
            plan.push_binding(*chain_id, *asset_id, Address::ZERO, 0, *perf_bps as u16);
        }
        plan
    }

    /// `ON CONFLICT DO UPDATE` refuses a statement that touches one key twice,
    /// so the plan must not hold two bindings for the same asset.
    #[test]
    fn a_repeated_binding_keeps_only_the_last() {
        let plan = plan_with(&[(1, 7, 10), (1, 7, 20)]);

        assert_eq!(plan.bindings.len(), 1);
        // The later event wins, as it would have writing them in sequence.
        assert_eq!(plan.bindings[0].perf_bps, 20);
    }

    #[test]
    fn bindings_for_different_assets_are_both_kept() {
        assert_eq!(plan_with(&[(1, 7, 10), (1, 8, 20)]).bindings.len(), 2);
    }

    /// A binding for another chain is a different row, even at the same asset id.
    #[test]
    fn bindings_are_scoped_by_chain() {
        assert_eq!(plan_with(&[(1, 7, 10), (2, 7, 20)]).bindings.len(), 2);
    }
}
