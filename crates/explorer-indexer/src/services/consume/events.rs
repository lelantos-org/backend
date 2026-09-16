//! Routing one decoded event into the row it writes.
//!
//! Pure: every function here builds a value and performs no IO. The rows are
//! collected into a [`CommitPlan`] and written in batches once the whole window
//! is decoded, rather than one statement — and one pool checkout — per event.

use super::flows::asset_moved;
use super::plan::CommitPlan;
use super::yield_fees::yield_fee;
use crate::repositories::yield_fee_events::{KIND_ACCRUED, KIND_SWEPT};
use chain_types::decode::DecodedEvent;
use database::RawEventRow;

/// Route one decoded event into the plan.
///
/// Pure and infallible: every arm builds a row and pushes it. What used to be a
/// per-event `await` on the database is now an append to a `Vec`, and the whole
/// window is written once by [`CommitPlan::apply`].
///
/// Most arms are empty. The fetch filter in `super::tick` keeps those kinds out,
/// so they never reach here — the arms exist because the match has no wildcard,
/// which is what makes a new event a compile error rather than a silent
/// omission.
pub fn plan_event(plan: &mut CommitPlan, chain_id: i64, row: &RawEventRow, event: DecodedEvent) {
    match event {
        DecodedEvent::AssetMoved {
            asset_id,
            token,
            in_amount,
            out_amount,
            public_in,
            public_out,
        } => plan.flows.push(asset_moved(
            chain_id, row, asset_id, token, in_amount, out_amount, public_in, public_out,
        )),
        DecodedEvent::PerfFeeAccrued {
            asset_id,
            units_minted,
            ..
        } => plan.yield_fees.push(yield_fee(
            chain_id,
            row,
            asset_id,
            KIND_ACCRUED,
            units_minted,
            None,
        )),
        DecodedEvent::NormalizedFeeSwept {
            asset_id,
            units,
            amount,
        } => plan.yield_fees.push(yield_fee(
            chain_id,
            row,
            asset_id,
            KIND_SWEPT,
            units,
            Some(amount),
        )),

        // Owned by fmd-indexer.
        DecodedEvent::NoteCreated { .. } => {}
        DecodedEvent::NullifierConsumed { .. } => {}

        // Owned by protocol-indexer: the asset catalog, the yield bindings, and
        // the two ledgers the relayer drains.
        DecodedEvent::AssetRegistered { .. } => {}
        DecodedEvent::AssetFeeSet { .. } => {}
        DecodedEvent::RootAdvanced { .. } => {}
        DecodedEvent::DepositEscrowed { .. } => {}
        DecodedEvent::DepositFlushed { .. } => {}
        DecodedEvent::DepositCanceled { .. } => {}
        DecodedEvent::YieldAssetAdded { .. } => {}
        DecodedEvent::YieldParamsSet { .. } => {}
        DecodedEvent::HaltedSet { .. } => {}

        // Write no derived state anywhere.
        DecodedEvent::Rebalanced { .. } => {}
        DecodedEvent::EmergencyUnwound { .. } => {}
    }
}
