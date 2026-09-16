//! The classification union: one SQL branch per kind, whole or trimmed per
//! branch to a page.

/// One branch of the classification union: its SQL, and the timestamp column it
/// is bounded and ordered by.
///
/// Split into an array rather than written out as one literal so the per-branch
/// `ORDER BY`/`LIMIT` that [`classified_top`] adds cannot be applied to three
/// branches and forgotten on the fourth.
pub(super) struct Branch {
    /// `$1` is the chain filter (NULL matches all) and `$2` the since-ts floor.
    sql: &'static str,
    /// Output column name, so the per-branch ordering names the same instant the
    /// outer ordering does even where the source column differs.
    ts: &'static str,
}

/// One row per classified operation, one branch per kind.
///
/// Shared by the feed and the bucketed counts so the two cannot disagree about
/// a kind.
pub(super) const BRANCHES: [Branch; 4] = [
    Branch {
        sql: "\
    SELECT f.chain_id, f.tx_hash, f.block_number, f.block_ts, f.log_index, \
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
           d.flushed_at_ts AS block_ts, d.flushed_log_index AS log_index, \
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
    SELECT d.chain_id, d.tx_hash, d.block_number, d.block_ts, d.log_index, \
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
    // `deposit_escrowed_flushed_tx_idx` exist, and the lateral is why
    // `tree_advances_chain_tx_idx` does: without them each is a scan of the
    // whole referenced table per candidate row.
    //
    // `r` bounds this operation's range by its neighbouring `RootAdvanced` logs
    // in the same transaction. A flush whose `flushed_log_index` is NULL was
    // recorded by an indexer that predates the column, when a flush always had
    // its transaction to itself, so it excludes by hash as before.
    Branch {
        sql: "\
    SELECT t.chain_id, t.tx_hash, t.block_number, t.block_ts, t.log_index, \
           'transfer' AS kind, NULL::BIGINT AS asset_id_u64, NULL::SMALLINT AS decimals, \
           NULL::NUMERIC AS amount, NULL::NUMERIC AS public_out \
      FROM tree_advances t \
     CROSS JOIN LATERAL ( \
           SELECT MAX(t2.log_index) FILTER (WHERE t2.log_index < t.log_index) AS prev_ra, \
                  MIN(t2.log_index) FILTER (WHERE t2.log_index > t.log_index) AS next_ra \
             FROM tree_advances t2 \
            WHERE t2.chain_id = t.chain_id AND t2.tx_hash = t.tx_hash) r \
     WHERE ($1::BIGINT IS NULL OR t.chain_id = $1) \
       AND t.block_ts >= $2 \
       AND NOT EXISTS ( \
             SELECT 1 FROM asset_flows f2 \
              WHERE f2.chain_id = t.chain_id AND f2.tx_hash = t.tx_hash \
                AND f2.log_index > t.log_index \
                AND (r.next_ra IS NULL OR f2.log_index < r.next_ra)) \
       AND NOT EXISTS ( \
             SELECT 1 FROM deposit_escrowed_events d2 \
              WHERE d2.chain_id = t.chain_id AND d2.flushed_tx_hash = t.tx_hash \
                AND (d2.flushed_log_index IS NULL \
                     OR (d2.flushed_log_index < t.log_index \
                         AND (r.prev_ra IS NULL OR d2.flushed_log_index > r.prev_ra))))",
        ts: "block_ts",
    },
];

/// The union, every branch whole. For aggregates, which read every row anyway.
pub(super) fn classified() -> String {
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
/// expression or an aliased column and the fourth carries a lateral and two
/// anti-joins, so there is no merge-append plan to find.
///
/// `log_index` sorts `NULLS LAST` in both sorts, which must stay identical: a
/// legacy flush with no recorded position ranks after the operations in its
/// block instead of ahead of them.
///
/// The result is identical. The outer sort picks the newest `$3` of at most
/// `4 * $3` candidates, and no discarded row could have outranked one kept: a
/// row trimmed inside its branch had `$3` newer rows in that same branch, all of
/// which are still present.
pub(super) fn classified_top() -> String {
    BRANCHES
        .iter()
        .enumerate()
        .map(|(i, b)| {
            format!(
                "SELECT * FROM ({} ORDER BY {} DESC, block_number DESC, log_index DESC NULLS LAST LIMIT $3) b{i}",
                b.sql, b.ts
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}
