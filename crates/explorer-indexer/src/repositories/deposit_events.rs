use crate::error::ExplorerIndexerError;
use bigdecimal::BigDecimal;
use database::DbPool;
use database::schema::deposit_escrowed_events;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = deposit_escrowed_events)]
pub struct NewDepositEscrowed {
    pub chain_id: i64,
    pub block_number: i64,
    pub log_index: i32,
    pub deposit_id: BigDecimal,
    pub payer: Vec<u8>,
    pub recipient: Vec<u8>,
    pub public_asset_id: i64,
    pub public_in: BigDecimal,
    pub fee_bps_at_submit: i32,
    pub cm: Vec<u8>,
    pub cv_dep_x: BigDecimal,
    pub cv_dep_y: BigDecimal,
    pub rcv: BigDecimal,
    pub aux: JsonValue,
    pub submitted_at_block: i64,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

pub async fn insert(pool: &DbPool, row: NewDepositEscrowed) -> Result<usize, ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    Ok(diesel::insert_into(deposit_escrowed_events::table)
        .values(&row)
        .on_conflict((
            deposit_escrowed_events::chain_id,
            deposit_escrowed_events::block_number,
            deposit_escrowed_events::log_index,
        ))
        .do_nothing()
        .execute(&mut conn)
        .await?)
}

pub async fn mark_flushed(
    pool: &DbPool,
    chain_id: i64,
    deposit_id: BigDecimal,
    flushed_at_block: i64,
) -> Result<usize, ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    Ok(diesel::update(
        deposit_escrowed_events::table
            .filter(deposit_escrowed_events::chain_id.eq(chain_id))
            .filter(deposit_escrowed_events::deposit_id.eq(deposit_id)),
    )
    .set(deposit_escrowed_events::flushed_at_block.eq(Some(flushed_at_block)))
    .execute(&mut conn)
    .await?)
}

pub async fn mark_canceled(
    pool: &DbPool,
    chain_id: i64,
    deposit_id: BigDecimal,
    canceled_at_block: i64,
) -> Result<usize, ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    Ok(diesel::update(
        deposit_escrowed_events::table
            .filter(deposit_escrowed_events::chain_id.eq(chain_id))
            .filter(deposit_escrowed_events::deposit_id.eq(deposit_id)),
    )
    .set(deposit_escrowed_events::canceled_at_block.eq(Some(canceled_at_block)))
    .execute(&mut conn)
    .await?)
}

pub async fn delete_from_block(
    pool: &DbPool,
    chain_id: i64,
    from_block: i64,
) -> Result<usize, ExplorerIndexerError> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ExplorerIndexerError::Db(e.to_string()))?;
    Ok(diesel::delete(
        deposit_escrowed_events::table
            .filter(deposit_escrowed_events::chain_id.eq(chain_id))
            .filter(deposit_escrowed_events::block_number.ge(from_block)),
    )
    .execute(&mut conn)
    .await?)
}
