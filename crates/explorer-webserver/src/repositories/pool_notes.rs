//! How many notes each chain's commitment tree holds.
//!
//! Context for the anonymity-set figures, not a privacy score of its own. The
//! total only ever grows, and it is an upper bound no single action achieves:
//! what covers a withdrawal is its denomination cohort, not the whole tree. It
//! is reported per chain because each chain has its own tree — notes on one
//! chain are no cover on another, so the totals must never be summed.

use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Nullable};
use diesel_async::RunQueryDsl;

/// One chain's tree occupancy.
#[derive(Debug, Clone, QueryableByName)]
pub struct PoolNotesRow {
    #[diesel(sql_type = BigInt)]
    pub chain_id: i64,
    /// Leaves committed to the tree: the contract's `committedCount`.
    #[diesel(sql_type = BigInt)]
    pub leaves: i64,
    /// Leaves belonging to relayer fee notes rather than to depositors.
    #[diesel(sql_type = BigInt)]
    pub fee_notes: i64,
    /// Newest advance behind the count, so a caller can age the figure.
    #[diesel(sql_type = BigInt)]
    pub last_ts: i64,
}

/// Tree occupancy per chain.
///
/// `MAX(start_index + inserted)` is the contract's `committedCount`, which
/// `CommitmentTree._advanceRoot` sets to exactly that. Taking the max rather
/// than `SUM(inserted)` means a gap in indexed advances under-reports rather
/// than silently double-counting a replayed one; the two agree on a complete
/// history, which is worth asserting when verifying an indexer run.
///
/// `fee_notes` counts flushed deposits. Every deposit occupies
/// `PubInputs.LEAVES_PER_DEPOSIT = 2` adjacent leaves — its principal and the
/// note paying whoever flushed it — and `flushBatch` advances the tree by
/// `2 * n` unconditionally, so one fee leaf exists per flushed deposit whatever
/// the fee was. Subtracting it gives the notes that actually belong to users.
/// Cancelled deposits never reach the tree, so they are excluded.
pub async fn per_chain(pool: &DbPool, chain_id: Option<i64>) -> AppResult<Vec<PoolNotesRow>> {
    let mut conn = super::conn(pool).await?;
    sql_query(
        "SELECT t.chain_id AS chain_id, \
                MAX(t.start_index + t.inserted)::BIGINT AS leaves, \
                COALESCE(( \
                    SELECT COUNT(*) FROM deposit_escrowed_events d \
                     WHERE d.chain_id = t.chain_id \
                       AND d.flushed_at_ts IS NOT NULL \
                       AND d.canceled_at_block IS NULL \
                ), 0)::BIGINT AS fee_notes, \
                MAX(t.block_ts) AS last_ts \
         FROM tree_advances t \
         WHERE ($1::BIGINT IS NULL OR t.chain_id = $1) \
         GROUP BY t.chain_id \
         ORDER BY t.chain_id",
    )
    .bind::<Nullable<BigInt>, _>(chain_id)
    .load(&mut conn)
    .await
    .map_err(|e| AppError::Db(e.to_string()))
}
