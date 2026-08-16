//! Transaction classification.
//!
//! Every MASP transaction falls into exactly one of four kinds, and the
//! contract makes the split exact rather than heuristic:
//!
//! - `AssetMoved` is emitted from two sites only — `withdraw()` emits
//!   `(0, outAmt)` and `_finalizeDeposit()` emits `(inAmt, 0)`. Both sides can
//!   never be non-zero: `withdraw` reverts on `publicIn != 0` and every spend
//!   entry point forces `publicIn == 0`. So the sign of an `asset_flows` row
//!   is the label.
//! - `RootAdvanced` is emitted from two sites only — `_finalize` (used by
//!   `withdraw` and `transfer`) and `flushBatch`.
//!
//! | tx                       | AssetMoved  | RootAdvanced | kind     |
//! |--------------------------|-------------|--------------|----------|
//! | deposit/depositAuthorized| `(in>0, 0)` | no           | pending  |
//! | …once flushed            |             | (the flush)  | deposit  |
//! | withdraw                 | `(0, out>0)`| yes          | withdraw |
//! | transfer                 | none        | yes          | transfer |
//!
//! A deposit counts at flush time, because that is when its note enters the
//! tree; until then it is `pending` at its escrow time. `DepositFlushed` is
//! emitted per deposit inside `flushBatch`, so a batch of eight counts as
//! eight deposits, not one.
//!
//! A flush transaction is therefore never a `transfer`, which is exactly what
//! `flushed_tx_hash` exists to guarantee.

use crate::domain::error::{AppError, AppResult};
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Bytea, Nullable, Numeric, SmallInt, Text};
use diesel_async::RunQueryDsl;

/// SQL producing one row per classified transaction. Shared by the feed and
/// the bucketed counts so the two can never disagree about a kind.
///
/// `$1` chain filter (NULL = all), `$2` since-ts filter (NULL = all).
const CLASSIFIED: &str = "\
    SELECT f.chain_id, f.tx_hash, f.block_number, f.block_ts, \
           'withdraw' AS kind, f.asset_id_u64, a.decimals, f.out_amount AS amount \
      FROM asset_flows f \
      JOIN assets a ON a.chain_id = f.chain_id AND a.asset_id_u64 = f.asset_id_u64 \
     WHERE f.out_amount > 0 \
       AND ($1::BIGINT IS NULL OR f.chain_id = $1) \
       AND ($2::BIGINT IS NULL OR f.block_ts >= $2) \
    UNION ALL \
    SELECT d.chain_id, d.flushed_tx_hash AS tx_hash, d.flushed_at_block AS block_number, \
           d.flushed_at_ts AS block_ts, \
           'deposit' AS kind, d.public_asset_id AS asset_id_u64, a.decimals, \
           (d.public_in * a.scale) AS amount \
      FROM deposit_escrowed_events d \
      JOIN assets a ON a.chain_id = d.chain_id AND a.asset_id_u64 = d.public_asset_id \
     WHERE d.flushed_at_ts IS NOT NULL AND d.canceled_at_block IS NULL \
       AND ($1::BIGINT IS NULL OR d.chain_id = $1) \
       AND ($2::BIGINT IS NULL OR d.flushed_at_ts >= $2) \
    UNION ALL \
    SELECT d.chain_id, d.tx_hash, d.block_number, d.block_ts, \
           'pending' AS kind, d.public_asset_id AS asset_id_u64, a.decimals, \
           (d.public_in * a.scale) AS amount \
      FROM deposit_escrowed_events d \
      JOIN assets a ON a.chain_id = d.chain_id AND a.asset_id_u64 = d.public_asset_id \
     WHERE d.flushed_at_block IS NULL AND d.canceled_at_block IS NULL \
       AND ($1::BIGINT IS NULL OR d.chain_id = $1) \
       AND ($2::BIGINT IS NULL OR d.block_ts >= $2) \
    UNION ALL \
    SELECT t.chain_id, t.tx_hash, t.block_number, t.block_ts, \
           'transfer' AS kind, NULL::BIGINT AS asset_id_u64, NULL::SMALLINT AS decimals, \
           NULL::NUMERIC AS amount \
      FROM tree_advances t \
     WHERE ($1::BIGINT IS NULL OR t.chain_id = $1) \
       AND ($2::BIGINT IS NULL OR t.block_ts >= $2) \
       AND NOT EXISTS ( \
             SELECT 1 FROM asset_flows f2 \
              WHERE f2.chain_id = t.chain_id AND f2.tx_hash = t.tx_hash) \
       AND NOT EXISTS ( \
             SELECT 1 FROM deposit_escrowed_events d2 \
              WHERE d2.chain_id = t.chain_id AND d2.flushed_tx_hash = t.tx_hash)";

#[derive(Debug, Clone, QueryableByName)]
pub struct ClassifiedTxRow {
    #[diesel(sql_type = BigInt)]
    pub chain_id: i64,
    #[diesel(sql_type = Bytea)]
    pub tx_hash: Vec<u8>,
    #[diesel(sql_type = BigInt)]
    pub block_number: i64,
    #[diesel(sql_type = BigInt)]
    pub block_ts: i64,
    #[diesel(sql_type = Text)]
    pub kind: String,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub asset_id_u64: Option<i64>,
    #[diesel(sql_type = Nullable<SmallInt>)]
    pub decimals: Option<i16>,
    /// Token base units. `NULL` for transfers, which move no public value.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub amount: Option<BigDecimal>,
}

/// Newest-first feed of classified transactions.
pub async fn recent(
    pool: &DbPool,
    chain_id: Option<i64>,
    since_ts: Option<i64>,
    limit: i64,
) -> AppResult<Vec<ClassifiedTxRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    sql_query(format!(
        "SELECT * FROM ({CLASSIFIED}) c ORDER BY c.block_ts DESC, c.block_number DESC LIMIT $3"
    ))
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<Nullable<BigInt>, _>(since_ts)
    .bind::<BigInt, _>(limit)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}

#[derive(Debug, Clone, QueryableByName)]
pub struct KindCountRow {
    #[diesel(sql_type = BigInt)]
    pub ts: i64,
    #[diesel(sql_type = Text)]
    pub kind: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Per-bucket transaction counts, one row per (bucket, kind).
pub async fn kind_counts(
    pool: &DbPool,
    chain_id: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Vec<KindCountRow>> {
    let mut conn = pool.get().await.map_err(|e| AppError::Db(e.to_string()))?;
    sql_query(format!(
        "SELECT (c.block_ts / $3) * $3 AS ts, c.kind, COUNT(*)::BIGINT AS count \
           FROM ({CLASSIFIED}) c \
          GROUP BY 1, 2 \
          ORDER BY 1"
    ))
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<Nullable<BigInt>, _>(since_ts)
    .bind::<BigInt, _>(bucket_sec)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
