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

use crate::DbPool;
use crate::schema::{chain_reorgs, consumer_cursors};
use diesel::prelude::*;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReorgError {
    #[error("pool: {0}")]
    Pool(String),
    #[error(transparent)]
    Query(#[from] diesel::result::Error),
}

pub type ReorgResult<T> = Result<T, ReorgError>;

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
pub async fn apply_pending(pool: &DbPool, name: &str, chain_id: i64) -> ReorgResult<usize> {
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
                let retracted = retract_derived(conn, chain_id, deepest).await?;
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

/// `DELETE FROM <table> WHERE chain_id = $1 AND block_number >= $2`.
///
/// A macro rather than a generic function: each table is a distinct diesel
/// type, so only the statement shape can be shared.
macro_rules! delete_at_or_above {
    ($conn:expr, $table:ident, $chain_id:expr, $from_block:expr) => {{
        use crate::schema::$table as t;
        diesel::delete(
            t::table
                .filter(t::chain_id.eq($chain_id))
                .filter(t::block_number.ge($from_block)),
        )
        .execute($conn)
        .await?
    }};
}

/// Delete every derived row for `chain_id` at or above `from_block`.
///
/// Idempotent: a re-run deletes nothing, so a consumer that crashes
/// mid-retraction can repeat it.
///
/// `matches` is omitted deliberately: `note_id REFERENCES notes(id) ON DELETE
/// CASCADE` removes it together with the notes.
async fn retract_derived(
    conn: &mut AsyncPgConnection,
    chain_id: i64,
    from_block: i64,
) -> Result<usize, diesel::result::Error> {
    let notes = delete_at_or_above!(conn, notes, chain_id, from_block);
    let spent = delete_at_or_above!(conn, spent_nullifiers, chain_id, from_block);
    let tree = delete_at_or_above!(conn, tree_advances, chain_id, from_block);
    let flows = delete_at_or_above!(conn, asset_flows, chain_id, from_block);
    let escrow = delete_at_or_above!(conn, deposit_escrowed_events, chain_id, from_block);
    Ok(notes + spent + tree + flows + escrow)
}

/// Rewind `name`'s cursor to the start and mark the reorg log processed to
/// `reorg_id`.
///
/// The cursor goes to id 0 rather than a computed id: once rows have been
/// re-inserted, `raw_events.id` is no longer ordered by block, so no id means
/// "just before this block". Replaying from the start is slower but correct,
/// and every consumer write is idempotent.
///
/// An upsert rather than an update: a consumer that has not committed yet has
/// no row, and an `UPDATE` matching nothing would leave `last_reorg_id`
/// unwritten, so [`apply_pending`] would rediscover the same reorg on every
/// call and spin the caller's tick loop. `(0, 0, reorg_id)` is both the correct
/// initial state and the meaning of a rewind: replay from the beginning with
/// this reorg applied.
async fn rewind_consumer(
    conn: &mut AsyncPgConnection,
    name: &str,
    chain_id: i64,
    reorg_id: i64,
) -> Result<(), diesel::result::Error> {
    diesel::insert_into(consumer_cursors::table)
        .values((
            consumer_cursors::name.eq(name),
            consumer_cursors::chain_id.eq(chain_id),
            consumer_cursors::last_event_id.eq(0i64),
            consumer_cursors::last_block_number.eq(0i64),
            consumer_cursors::last_reorg_id.eq(reorg_id),
        ))
        .on_conflict((consumer_cursors::name, consumer_cursors::chain_id))
        .do_update()
        .set((
            consumer_cursors::last_event_id.eq(0i64),
            consumer_cursors::last_block_number.eq(0i64),
            consumer_cursors::last_reorg_id.eq(reorg_id),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

async fn checkout(
    pool: &DbPool,
) -> ReorgResult<
    bb8::PooledConnection<
        '_,
        diesel_async::pooled_connection::AsyncDieselConnectionManager<AsyncPgConnection>,
    >,
> {
    pool.get()
        .await
        .map_err(|e| ReorgError::Pool(e.to_string()))
}
