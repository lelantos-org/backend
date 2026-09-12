//! Routing one decoded event into the row it writes.
//!
//! Pure: every function here builds a value and performs no IO. The rows are
//! collected into a [`CommitPlan`] and written in batches once the whole window
//! is decoded, rather than one statement — and one pool checkout — per event.

use super::plan::CommitPlan;
use crate::repositories::{
    asset_flows::NewAssetFlow,
    yield_fee_events::{KIND_ACCRUED, KIND_SWEPT, NewYieldFeeEvent},
};
use alloy::primitives::{Address, U256};
use bigdecimal::BigDecimal;
use chain_types::decode::DecodedEvent;
use chain_types::numeric::u256_to_bigdecimal;
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

// One argument per `DecodedEvent::AssetMoved` field; grouping them would only
// restate the variant.
#[allow(clippy::too_many_arguments)]
fn asset_moved(
    chain_id: i64,
    row: &RawEventRow,
    asset_id: u64,
    token: Address,
    in_amount: U256,
    out_amount: U256,
    public_in: u64,
    public_out: u64,
) -> NewAssetFlow {
    NewAssetFlow {
        chain_id,
        block_number: row.block_number,
        log_index: row.log_index,
        asset_id_u64: asset_id as i64,
        token: token.as_slice().to_vec(),
        in_amount: u256_to_bigdecimal(in_amount),
        out_amount: u256_to_bigdecimal(out_amount),
        tx_hash: row.tx_hash.clone(),
        block_ts: row.block_ts,
        public_in: Some(BigDecimal::from(public_in)),
        public_out: Some(BigDecimal::from(public_out)),
    }
}

/// A treasury fee event, accrued or swept.
///
/// One function for both because they differ only in `kind` and whether tokens
/// moved: an accrual mints units to the treasury and moves nothing, which is
/// why `amount` is `None` there and why this log is the only trace of it.
fn yield_fee(
    chain_id: i64,
    row: &RawEventRow,
    asset_id: u64,
    kind: i16,
    units: U256,
    amount: Option<U256>,
) -> NewYieldFeeEvent {
    NewYieldFeeEvent {
        chain_id,
        asset_id_u64: asset_id as i64,
        block_number: row.block_number,
        block_ts: row.block_ts,
        tx_hash: row.tx_hash.clone(),
        log_index: row.log_index,
        kind,
        units: u256_to_bigdecimal(units),
        amount: amount.map(u256_to_bigdecimal),
    }
}
