use crate::domain::error::ProtocolIndexerError;
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
    /// The relayer's fee note, the deposit's second leaf. Part of the digest
    /// preimage: the relayer rebuilds `MASP._depositDigest` from these, so they
    /// are stored exactly as logged rather than normalised. `fee_asset_id` is
    /// the fee note's asset, independent of `public_asset_id` and 0 for a zero
    /// fee.
    pub fee_asset_id: i64,
    pub fee_in: BigDecimal,
    pub fee_cm: Vec<u8>,
    pub fee_cv_dep_x: BigDecimal,
    pub fee_cv_dep_y: BigDecimal,
    pub fee_rcv: BigDecimal,
    pub fee_aux: JsonValue,
    pub submitted_at_block: i64,
    pub tx_hash: Vec<u8>,
    pub block_ts: i64,
}

/// Insert a whole tick's escrow events in one statement.
///
/// `ON CONFLICT DO NOTHING` on `(chain_id, block_number, log_index)`, so a
/// replayed window converges instead of erroring.
pub async fn insert_batch(
    pool: &DbPool,
    rows: &[NewDepositEscrowed],
) -> Result<usize, ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut conn = super::conn(pool).await?;
    Ok(diesel::insert_into(deposit_escrowed_events::table)
        .values(rows)
        .on_conflict((
            deposit_escrowed_events::chain_id,
            deposit_escrowed_events::block_number,
            deposit_escrowed_events::log_index,
        ))
        .do_nothing()
        .execute(&mut conn)
        .await?)
}

/// One flush, as [`mark_flushed_batch`] applies it.
///
/// Records the flush together with the transaction and log that performed it.
///
/// The position separates a `flushBatch` from a `transfer` downstream: both
/// advance the tree and move no tokens. Block number alone is insufficient,
/// since one block can hold both, and so is the tx hash, since one `Bundler`
/// transaction can hold both.
#[derive(Debug, Clone)]
pub struct MarkFlushed {
    pub chain_id: i64,
    pub deposit_id: BigDecimal,
    pub block_number: i64,
    pub block_ts: i64,
    pub tx_hash: Vec<u8>,
    /// The `DepositFlushed` log's index within `tx_hash`.
    pub log_index: i32,
}

/// One cancellation, as [`mark_canceled_batch`] applies it.
#[derive(Debug, Clone)]
pub struct MarkCanceled {
    pub chain_id: i64,
    pub deposit_id: BigDecimal,
    pub block_number: i64,
}

/// Apply a tick's flushes over one pooled connection.
///
/// Still one `UPDATE` per row — each names a different `deposit_id` and sets
/// different values, so there is no single statement to collapse them into
/// without a `FROM (VALUES ...)` join. What this removes is the pool checkout
/// per event, which was the dominant cost; flushes are also far rarer than the
/// inserts, since one `flushBatch` retires a whole queue.
///
/// Applied in event order, so two events touching one deposit land in the order
/// the chain emitted them.
pub async fn mark_flushed_batch(
    pool: &DbPool,
    rows: &[MarkFlushed],
) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        diesel::update(
            deposit_escrowed_events::table
                .filter(deposit_escrowed_events::chain_id.eq(row.chain_id))
                .filter(deposit_escrowed_events::deposit_id.eq(row.deposit_id.clone())),
        )
        .set((
            deposit_escrowed_events::flushed_at_block.eq(Some(row.block_number)),
            deposit_escrowed_events::flushed_at_ts.eq(Some(row.block_ts)),
            deposit_escrowed_events::flushed_tx_hash.eq(Some(row.tx_hash.clone())),
            deposit_escrowed_events::flushed_log_index.eq(Some(row.log_index)),
        ))
        .execute(&mut conn)
        .await?;
    }
    Ok(())
}

/// Apply a tick's cancellations over one pooled connection; see
/// [`mark_flushed_batch`].
pub async fn mark_canceled_batch(
    pool: &DbPool,
    rows: &[MarkCanceled],
) -> Result<(), ProtocolIndexerError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = super::conn(pool).await?;
    for row in rows {
        diesel::update(
            deposit_escrowed_events::table
                .filter(deposit_escrowed_events::chain_id.eq(row.chain_id))
                .filter(deposit_escrowed_events::deposit_id.eq(row.deposit_id.clone())),
        )
        .set(deposit_escrowed_events::canceled_at_block.eq(Some(row.block_number)))
        .execute(&mut conn)
        .await?;
    }
    Ok(())
}

pub async fn delete_from_block(
    pool: &DbPool,
    chain_id: i64,
    from_block: i64,
) -> Result<usize, ProtocolIndexerError> {
    let mut conn = super::conn(pool).await?;
    Ok(diesel::delete(
        deposit_escrowed_events::table
            .filter(deposit_escrowed_events::chain_id.eq(chain_id))
            .filter(deposit_escrowed_events::block_number.ge(from_block)),
    )
    .execute(&mut conn)
    .await?)
}
