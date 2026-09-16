//! `PerfFeeAccrued` and `NormalizedFeeSwept` into their `yield_fee_events` rows.

use crate::repositories::yield_fee_events::NewYieldFeeEvent;
use alloy::primitives::U256;
use chain_types::numeric::u256_to_bigdecimal;
use database::RawEventRow;

/// A treasury fee event, accrued or swept.
///
/// One function for both because they differ only in `kind` and whether tokens
/// moved: an accrual mints units to the treasury and moves nothing, which is
/// why `amount` is `None` there and why this log is the only trace of it.
pub(super) fn yield_fee(
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
