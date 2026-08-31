use crate::error::ExplorerIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::yield_fee_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Units minted to the treasury. Moves no tokens, so `amount` is `None`.
pub const KIND_ACCRUED: i16 = 1;
/// Units burned and paid out, differing by the index at settlement.
pub const KIND_SWEPT: i16 = 2;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = yield_fee_events)]
pub struct NewYieldFeeEvent {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub block_number: i64,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    pub log_index: i32,
    pub kind: i16,
    pub units: BigDecimal,
    pub amount: Option<BigDecimal>,
}

/// `do_nothing` on the `(chain_id, tx_hash, log_index)` unique index: a cursor
/// rewind re-reads the same logs, and a fee event is a fact about one log.
pub async fn insert(pool: &DbPool, row: NewYieldFeeEvent) -> Result<(), ExplorerIndexerError> {
    let mut conn = super::conn(pool).await?;
    diesel::insert_into(yield_fee_events::table)
        .values(&row)
        .on_conflict_do_nothing()
        .execute(&mut conn)
        .await?;
    Ok(())
}
