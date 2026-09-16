//! `AssetMoved` into its `asset_flows` row.

use crate::repositories::asset_flows::NewAssetFlow;
use alloy::primitives::{Address, U256};
use bigdecimal::BigDecimal;
use chain_types::numeric::u256_to_bigdecimal;
use database::RawEventRow;

// One argument per `DecodedEvent::AssetMoved` field; grouping them would only
// restate the variant.
#[allow(clippy::too_many_arguments)]
pub(super) fn asset_moved(
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
