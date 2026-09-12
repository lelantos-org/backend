//! Transaction classification.
//!
//! Every MASP transaction falls into exactly one of four kinds, and the contract
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
//! | tx                       | AssetMoved  | RootAdvanced | kind     |
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
//! A flush transaction is therefore never a `transfer`, which `flushed_tx_hash`
//! guarantees.

use crate::domain::error::AppResult;
use bigdecimal::BigDecimal;
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Bytea, Nullable, Numeric, SmallInt, Text};
use diesel_async::RunQueryDsl;

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

/// One branch of the classification union: its SQL, and the timestamp column it
/// is bounded and ordered by.
///
/// Split into an array rather than written out as one literal so the per-branch
/// `ORDER BY`/`LIMIT` that [`classified_top`] adds cannot be applied to three
/// branches and forgotten on the fourth.
struct Branch {
    /// `$1` is the chain filter (NULL matches all) and `$2` the since-ts floor.
    sql: &'static str,
    /// Output column name, so the per-branch ordering names the same instant the
    /// outer ordering does even where the source column differs.
    ts: &'static str,
}

/// One row per classified transaction, one branch per kind.
///
/// Shared by the feed and the bucketed counts so the two cannot disagree about
/// a kind.
const BRANCHES: [Branch; 4] = [
    Branch {
        sql: "\
    SELECT f.chain_id, f.tx_hash, f.block_number, f.block_ts, \
           'withdraw' AS kind, f.asset_id_u64, a.decimals, f.out_amount AS amount, \
           f.public_out AS public_out \
      FROM asset_flows f \
      JOIN assets a ON a.chain_id = f.chain_id AND a.asset_id_u64 = f.asset_id_u64 \
     WHERE f.out_amount > 0 \
       AND ($1::BIGINT IS NULL OR f.chain_id = $1) \
       AND f.block_ts >= $2",
        ts: "block_ts",
    },
    Branch {
        sql: "\
    SELECT d.chain_id, d.flushed_tx_hash AS tx_hash, d.flushed_at_block AS block_number, \
           d.flushed_at_ts AS block_ts, \
           'deposit' AS kind, d.public_asset_id AS asset_id_u64, a.decimals, \
           (d.public_in * a.scale) AS amount, NULL::NUMERIC AS public_out \
      FROM deposit_escrowed_events d \
      JOIN assets a ON a.chain_id = d.chain_id AND a.asset_id_u64 = d.public_asset_id \
     WHERE d.flushed_at_ts IS NOT NULL AND d.canceled_at_block IS NULL \
       AND ($1::BIGINT IS NULL OR d.chain_id = $1) \
       AND d.flushed_at_ts >= $2",
        ts: "block_ts",
    },
    Branch {
        sql: "\
    SELECT d.chain_id, d.tx_hash, d.block_number, d.block_ts, \
           'pending' AS kind, d.public_asset_id AS asset_id_u64, a.decimals, \
           (d.public_in * a.scale) AS amount, NULL::NUMERIC AS public_out \
      FROM deposit_escrowed_events d \
      JOIN assets a ON a.chain_id = d.chain_id AND a.asset_id_u64 = d.public_asset_id \
     WHERE d.flushed_at_block IS NULL AND d.canceled_at_block IS NULL \
       AND ($1::BIGINT IS NULL OR d.chain_id = $1) \
       AND d.block_ts >= $2",
        ts: "block_ts",
    },
    // The anti-joins are why `asset_flows_chain_tx_idx` and
    // `deposit_escrowed_flushed_tx_idx` exist: without them each is a scan of
    // the whole referenced table per candidate row.
    Branch {
        sql: "\
    SELECT t.chain_id, t.tx_hash, t.block_number, t.block_ts, \
           'transfer' AS kind, NULL::BIGINT AS asset_id_u64, NULL::SMALLINT AS decimals, \
           NULL::NUMERIC AS amount, NULL::NUMERIC AS public_out \
      FROM tree_advances t \
     WHERE ($1::BIGINT IS NULL OR t.chain_id = $1) \
       AND t.block_ts >= $2 \
       AND NOT EXISTS ( \
             SELECT 1 FROM asset_flows f2 \
              WHERE f2.chain_id = t.chain_id AND f2.tx_hash = t.tx_hash) \
       AND NOT EXISTS ( \
             SELECT 1 FROM deposit_escrowed_events d2 \
              WHERE d2.chain_id = t.chain_id AND d2.flushed_tx_hash = t.tx_hash)",
        ts: "block_ts",
    },
];

/// The union, every branch whole. For aggregates, which read every row anyway.
fn classified() -> String {
    BRANCHES
        .iter()
        .map(|b| b.sql)
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// The union with each branch pre-trimmed to its own newest `$3` rows.
///
/// Without this the outer `ORDER BY ... LIMIT` had to sort the whole union — all
/// four branches over the whole window — to return one page. Postgres will not
/// push a limit through `UNION ALL` on its own here: three branches order by an
/// expression or an aliased column and the fourth carries two anti-joins, so
/// there is no merge-append plan to find.
///
/// The result is identical. The outer sort picks the newest `$3` of at most
/// `4 * $3` candidates, and no discarded row could have outranked one kept: a
/// row trimmed inside its branch had `$3` newer rows in that same branch, all of
/// which are still present.
fn classified_top() -> String {
    BRANCHES
        .iter()
        .enumerate()
        .map(|(i, b)| {
            format!(
                "SELECT * FROM ({} ORDER BY {} DESC, block_number DESC LIMIT $3) b{i}",
                b.sql, b.ts
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

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
    /// The circuit value a withdrawal published, in circuit units. `NULL` for
    /// every other kind, and for withdrawals indexed before the contract emitted
    /// the field — the two are indistinguishable here and both mean "no
    /// denomination to report", never a denomination of zero.
    #[diesel(sql_type = Nullable<Numeric>)]
    pub public_out: Option<BigDecimal>,
}

/// Newest-first feed of classified transactions.
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
          ORDER BY c.block_ts DESC, c.block_number DESC LIMIT $3"
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
mod tests {
    use super::*;

    /// The trimmed union must carry every branch the untrimmed one does. A kind
    /// present in one and absent from the other would make the feed and the
    /// counts disagree about what exists.
    #[test]
    fn both_unions_cover_every_kind() {
        let (plain, top) = (classified(), classified_top());
        for kind in ["'withdraw'", "'deposit'", "'pending'", "'transfer'"] {
            assert!(plain.contains(kind), "classified() is missing {kind}");
            assert!(top.contains(kind), "classified_top() is missing {kind}");
        }
        assert_eq!(plain.matches("UNION ALL").count(), BRANCHES.len() - 1);
        assert_eq!(top.matches("UNION ALL").count(), BRANCHES.len() - 1);
    }

    /// Every branch is trimmed, not merely the first. This is the whole reason
    /// the branches are an array: a hand-written union grows a fifth branch
    /// without the tail.
    #[test]
    fn every_branch_is_trimmed_by_the_limit() {
        let top = classified_top();
        assert_eq!(top.matches("LIMIT $3").count(), BRANCHES.len());
        assert_eq!(top.matches("ORDER BY").count(), BRANCHES.len());
    }

    /// The aggregate reads every row in the window, so a per-branch limit there
    /// would silently undercount.
    #[test]
    fn the_aggregate_union_is_untrimmed() {
        let plain = classified();
        assert!(!plain.contains("LIMIT"), "{plain}");
        assert!(!plain.contains("ORDER BY"), "{plain}");
    }

    /// Every branch must be bounded by the since-ts floor, or one kind would
    /// scan all history while the others honour the window.
    #[test]
    fn every_branch_is_bounded_by_the_since_floor() {
        assert_eq!(classified().matches(">= $2").count(), BRANCHES.len());
    }

    #[test]
    fn an_explicit_since_is_passed_through() {
        assert_eq!(since_or_default(Some(1_700_000_000)), 1_700_000_000);
    }

    /// `0` is how a caller asks for all history; it must not be mistaken for
    /// absent and replaced by the default window.
    #[test]
    fn zero_asks_for_all_history() {
        assert_eq!(since_or_default(Some(0)), 0);
        assert_eq!(since_or_default(Some(-5)), 0);
    }

    #[test]
    fn an_absent_since_defaults_to_the_window() {
        let now = chrono::Utc::now().timestamp();
        let got = since_or_default(None);
        assert!(
            got <= now - DEFAULT_WINDOW_SEC,
            "{got} is inside the window"
        );
        // Bounded on the other side too, so a wrong sign cannot pass.
        assert!(got > now - DEFAULT_WINDOW_SEC - 60, "{got} is too far back");
    }
}
