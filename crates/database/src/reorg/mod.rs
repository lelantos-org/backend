//! Reorg retraction shared by every consumer of `raw_events`.
//!
//! The ingester deletes `raw_events` rows for blocks a fork took away and
//! re-ingests the canonical replacements. Consumers stream `raw_events` by
//! ascending `id` and the replacements receive fresh, higher `BIGSERIAL` ids,
//! so replay is automatic.
//!
//! State already derived from the deleted rows is not. Those rows sit below the
//! consumer's cursor, so nothing revisits them, and the notes, nullifiers and
//! flows they produced continue to describe an abandoned branch.
//!
//! [`apply_pending`] covers that case: given the block a fork started at, it
//! drops everything derived at or above it and rewinds the consumer so replay
//! rebuilds the state.

mod retract;

use crate::schema::{chain_reorgs, consumer_cursors};
use crate::{DbConn, DbPool};
use diesel::prelude::*;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use retract::{retract_derived, rewind_consumer};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReorgError {
    #[error("pool: {0}")]
    Pool(String),
    #[error(transparent)]
    Query(#[from] diesel::result::Error),
}

pub type ReorgResult<T> = Result<T, ReorgError>;

/// A consumer of `raw_events`, identified by the derived state it owns.
///
/// Names both the cursor row and the tables a retraction deletes, so the two
/// cannot disagree. That pairing is the invariant retraction depends on: a
/// consumer whose table is deleted must also have its cursor rewound, or the
/// rows are never rebuilt.
///
/// Deleting only what the caller owns is what makes several consumers safe.
/// Before the split every retraction deleted every derived table, which worked
/// only because each consumer independently reached the same reorg record and
/// replayed — leaving a window in which one consumer's tables were missing with
/// only another's cursor rewound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// Notes, spent nullifiers and the tree frontier.
    Fmd,
    /// The asset catalog, the yield bindings, and the two ledgers the relayer
    /// transacts against.
    Protocol,
    /// Flow analytics for explorer-ui.
    Explorer,
}

impl Owner {
    /// The `consumer_cursors.name` this owner commits under.
    pub const fn cursor_name(self) -> &'static str {
        match self {
            Owner::Fmd => "fmd",
            Owner::Protocol => "protocol",
            Owner::Explorer => "explorer",
        }
    }
}

/// One recorded rewind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Queryable)]
pub struct ReorgRecord {
    pub id: i64,
    pub chain_id: i64,
    /// First discarded block. Everything at or above it is re-derived.
    pub rewind_to: i64,
}

/// Record a rewind.
///
/// Takes a connection rather than the pool because the caller must run this
/// inside the transaction that deletes the rows; the marker and the deletion
/// have to commit together.
pub async fn record(
    conn: &mut AsyncPgConnection,
    chain_id: i64,
    rewind_to: i64,
) -> Result<i64, diesel::result::Error> {
    diesel::insert_into(chain_reorgs::table)
        .values((
            chain_reorgs::chain_id.eq(chain_id),
            chain_reorgs::rewind_to.eq(rewind_to),
        ))
        .returning(chain_reorgs::id)
        .get_result(conn)
        .await
}

/// Reorgs for `chain_id` newer than `after_id`, oldest first.
pub async fn pending(pool: &DbPool, chain_id: i64, after_id: i64) -> ReorgResult<Vec<ReorgRecord>> {
    let mut conn = checkout(pool).await?;
    Ok(chain_reorgs::table
        .filter(chain_reorgs::chain_id.eq(chain_id))
        .filter(chain_reorgs::id.gt(after_id))
        .order(chain_reorgs::id.asc())
        .select((
            chain_reorgs::id,
            chain_reorgs::chain_id,
            chain_reorgs::rewind_to,
        ))
        .load::<ReorgRecord>(&mut conn)
        .await?)
}

/// The reorg log position this consumer has already applied.
pub async fn consumer_position(pool: &DbPool, name: &str, chain_id: i64) -> ReorgResult<i64> {
    let mut conn = checkout(pool).await?;
    Ok(consumer_cursors::table
        .filter(consumer_cursors::name.eq(name))
        .filter(consumer_cursors::chain_id.eq(chain_id))
        .select(consumer_cursors::last_reorg_id)
        .first::<i64>(&mut conn)
        .await
        .optional()?
        .unwrap_or(0))
}

/// Apply every unprocessed reorg for one consumer.
///
/// Returns the number applied; `0` means nothing was pending, the common case,
/// and costs one indexed lookup.
///
/// Retracts from the lowest `rewind_to` across the batch: several forks can be
/// pending at once and the deepest bounds what must be rebuilt. Retraction and
/// the cursor rewind share a transaction, so no reader sees derived rows
/// removed while the cursor still reports being past them.
pub async fn apply_pending(pool: &DbPool, owner: Owner, chain_id: i64) -> ReorgResult<usize> {
    let name = owner.cursor_name();
    let after = consumer_position(pool, name, chain_id).await?;
    let pending = pending(pool, chain_id, after).await?;
    let (Some(deepest), Some(latest)) = (
        pending.iter().map(|r| r.rewind_to).min(),
        pending.iter().map(|r| r.id).max(),
    ) else {
        return Ok(0);
    };

    let mut conn = checkout(pool).await?;
    let retracted = conn
        .transaction::<_, diesel::result::Error, _>(|conn| {
            async move {
                let retracted = retract_derived(conn, owner, chain_id, deepest).await?;
                rewind_consumer(conn, name, chain_id, latest).await?;
                Ok(retracted)
            }
            .scope_boxed()
        })
        .await?;

    tracing::warn!(
        chain_id,
        consumer = name,
        reorgs = pending.len(),
        from_block = deepest,
        retracted,
        "retracted derived state after a reorg; consumer will replay"
    );
    Ok(pending.len())
}

async fn checkout(pool: &DbPool) -> ReorgResult<DbConn<'_>> {
    pool.get()
        .await
        .map_err(|e| ReorgError::Pool(e.to_string()))
}
