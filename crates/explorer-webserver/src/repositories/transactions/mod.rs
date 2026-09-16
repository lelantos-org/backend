//! Transaction classification.
//!
//! Every MASP operation falls into exactly one of four kinds, and the contract
//! makes the split exact rather than heuristic:
//!
//! - `AssetMoved` is emitted from two sites only: `withdraw()` emits
//!   `(0, outAmt)` and `_finalizeDeposit()` emits `(inAmt, 0)`. Both sides can
//!   never be non-zero, since `withdraw` reverts on `publicIn != 0` and every
//!   spend entry point forces `publicIn == 0`, so the sign of an `asset_flows`
//!   row determines the label.
//! - `RootAdvanced` is emitted from two sites only: `_finalize` (used by
//!   `withdraw` and `transfer`) and `flushBatch`.
//!
//! | operation                | AssetMoved  | RootAdvanced | kind     |
//! |--------------------------|-------------|--------------|----------|
//! | deposit/depositAuthorized| `(in>0, 0)` | no           | pending  |
//! | …once flushed            |             | (the flush)  | deposit  |
//! | withdraw                 | `(0, out>0)`| yes          | withdraw |
//! | transfer                 | none        | yes          | transfer |
//!
//! A deposit counts at flush time, when its note enters the tree; until then it
//! is `pending` at its escrow time. `DepositFlushed` is emitted per deposit
//! inside `flushBatch`, so a batch of eight counts as eight deposits.
//!
//! Kinds are exclusive per operation, not per transaction: a `Bundler`
//! transaction lands several operations, in any mix, under one hash. Each
//! operation owns exactly one `RootAdvanced`, and its other logs sit around it
//! in a fixed layout (pinned by `Bundler.t.sol::test_execute_mixedBundle_logLayout`):
//!
//! - a flush's `DepositFlushed` logs come after the previous `RootAdvanced` in
//!   the transaction (or its start) and before its own;
//! - a withdrawal's `AssetMoved` comes after its `RootAdvanced` and before the
//!   next one (or the transaction's end).
//!
//! So a `RootAdvanced` is a `transfer` when neither range holds such a log.
//! Ranges rather than `log_index + 1` adjacency, because token `Transfer` logs
//! interleave. Rows carry the `log_index` that identifies their operation within
//! the transaction.

mod union;

use crate::domain::error::AppResult;
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Bytea, Integer, Nullable, Numeric, SmallInt, Text};
use diesel_async::RunQueryDsl;
use union::{classified, classified_top};

/// Default lookback when the caller names no `sinceTs`.
///
/// Applied in SQL rather than in the handler so the cache key stays `None`: a
/// default resolved per request would be a fresh absolute timestamp every
/// second, and every request would miss.
///
/// The window exists because both consumers were previously unbounded — the feed
/// sorted all history to return one page, and the counts aggregated all history
/// on every miss. `sinceTs=0` still asks for everything, so nothing a caller
/// could express has been taken away; only the default changed.
const DEFAULT_WINDOW_SEC: i64 = 30 * 86_400;

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
    /// Identifies the operation within `tx_hash`, which a `Bundler` transaction
    /// shares across several: the `AssetMoved` of a withdrawal, the
    /// `DepositFlushed` of a deposit, the `DepositEscrowed` of a pending deposit
    /// and the `RootAdvanced` of a transfer. `NULL` only for a deposit flushed
    /// before the indexer recorded the position.
    #[diesel(sql_type = Nullable<Integer>)]
    pub log_index: Option<i32>,
    #[diesel(sql_type = Text)]
    pub kind: String,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub asset_id_u64: Option<i64>,
    #[diesel(sql_type = Nullable<SmallInt>)]
    pub decimals: Option<i16>,
    /// Token base units. `NULL` for transfers, which move no public value.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub amount: Option<BigDecimal>,
    /// The circuit value a withdrawal published, in circuit units. `NULL` for
    /// every other kind, and for withdrawals indexed before the contract emitted
    /// the field — the two are indistinguishable here and both mean "no
    /// denomination to report", never a denomination of zero.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub public_out: Option<BigDecimal>,
}

/// Newest-first feed of classified operations.
///
/// `kind` filters outside the union rather than inside it: a row's kind is
/// decided by the branch that produced it, so the classified result is the only
/// place all four are comparable. `LIMIT` applies after that filter, so a
/// filtered feed is a full page of one kind rather than the remainder of a mixed
/// page.
pub async fn recent(
    pool: &DbPool,
    chain_id: Option<i64>,
    since_ts: Option<i64>,
    kind: Option<&str>,
    limit: i64,
) -> AppResult<Vec<ClassifiedTxRow>> {
    let mut conn = super::conn(pool).await?;
    let union = classified_top();
    sql_query(format!(
        "SELECT * FROM ({union}) c \
          WHERE ($4::TEXT IS NULL OR c.kind = $4) \
          ORDER BY c.block_ts DESC, c.block_number DESC, c.log_index DESC NULLS LAST LIMIT $3"
    ))
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<BigInt, _>(since_or_default(since_ts))
    .bind::<BigInt, _>(limit)
    .bind::<Nullable<Text>, _>(kind)
    .load(&mut conn)
    .await
    .map_err(super::db_err)
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
    let mut conn = super::conn(pool).await?;
    let union = classified();
    sql_query(format!(
        "SELECT (c.block_ts / $3) * $3 AS ts, c.kind, COUNT(*)::BIGINT AS count \
           FROM ({union}) c \
          GROUP BY 1, 2 \
          ORDER BY 1"
    ))
    .bind::<Nullable<BigInt>, _>(chain_id)
    .bind::<BigInt, _>(since_or_default(since_ts))
    .bind::<BigInt, _>(bucket_sec)
    .load(&mut conn)
    .await
    .map_err(super::db_err)
}

/// The caller's floor, or [`DEFAULT_WINDOW_SEC`] back from now.
///
/// Negative values are floored at zero rather than rejected: a negative epoch
/// asks for everything, which `0` already expresses, and there is nothing for a
/// 400 to tell the caller they did not already mean.
fn since_or_default(since_ts: Option<i64>) -> i64 {
    match since_ts {
        Some(ts) => ts.max(0),
        None => (chrono::Utc::now().timestamp() - DEFAULT_WINDOW_SEC).max(0),
    }
}

#[cfg(test)]
mod tests;
