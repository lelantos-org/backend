//! The per-owner deletions and the cursor rewind that [`super::apply_pending`]
//! runs in one transaction.

use super::Owner;
use crate::schema::consumer_cursors;
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

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

/// Delete `owner`'s derived rows for `chain_id` at or above `from_block`.
///
/// Scoped to the caller. A consumer deletes only what it writes, so the tables
/// it drops are exactly the ones its own cursor rewind will rebuild.
///
/// Idempotent: a re-run deletes nothing, so a consumer that crashes
/// mid-retraction can repeat it.
///
/// `matches` is omitted deliberately: `note_id REFERENCES notes(id) ON DELETE
/// CASCADE` removes it together with the notes.
///
/// `assets` and `asset_yield` appear under no owner. They hold current state
/// rather than per-block rows and self-heal: the polled columns are overwritten
/// on the next tick, the event-sourced ones on the cursor rewind. A registration
/// is an idempotent fact and survives a fork.
pub(super) async fn retract_derived(
    conn: &mut AsyncPgConnection,
    owner: Owner,
    chain_id: i64,
    from_block: i64,
) -> Result<usize, diesel::result::Error> {
    Ok(match owner {
        Owner::Fmd => {
            let notes = delete_at_or_above!(conn, notes, chain_id, from_block);
            let spent = delete_at_or_above!(conn, spent_nullifiers, chain_id, from_block);

            // `tree_state` holds one current row rather than per-block rows, so
            // it cannot be trimmed to a block: its frontier already commits to
            // the leaves being deleted above. Dropping it is correct because the
            // cursor rewind below replays the chain from the start, and
            // fmd-indexer rebuilds the row as it re-commits. Leaving it would
            // keep serving a root for notes that no longer exist -- the one
            // failure this table must not have.
            let tree_st = diesel::delete(
                crate::schema::tree_state::table
                    .filter(crate::schema::tree_state::chain_id.eq(chain_id)),
            )
            .execute(conn)
            .await?;

            notes + spent + tree_st
        }
        Owner::Protocol => {
            use crate::schema::deposit_escrowed_events as d;
            let tree = delete_at_or_above!(conn, tree_advances, chain_id, from_block);
            let escrow = delete_at_or_above!(conn, deposit_escrowed_events, chain_id, from_block);

            // An escrow older than the fork survives the delete above, but a
            // flush or cancel of it inside the fork does not exist any more. The
            // replay re-marks the ones the new chain still has; left set, the
            // rest would hide a still-pending deposit from the flush worker.
            let flushed = diesel::update(
                d::table
                    .filter(d::chain_id.eq(chain_id))
                    .filter(d::flushed_at_block.ge(from_block)),
            )
            .set((
                d::flushed_at_block.eq(None::<i64>),
                d::flushed_at_ts.eq(None::<i64>),
                d::flushed_tx_hash.eq(None::<Vec<u8>>),
                d::flushed_log_index.eq(None::<i32>),
            ))
            .execute(conn)
            .await?;
            let canceled = diesel::update(
                d::table
                    .filter(d::chain_id.eq(chain_id))
                    .filter(d::canceled_at_block.ge(from_block)),
            )
            .set(d::canceled_at_block.eq(None::<i64>))
            .execute(conn)
            .await?;

            tree + escrow + flushed + canceled
        }
        Owner::Explorer => {
            let flows = delete_at_or_above!(conn, asset_flows, chain_id, from_block);
            let yield_fees = delete_at_or_above!(conn, yield_fee_events, chain_id, from_block);
            flows + yield_fees
        }
    })
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
/// unwritten, so [`super::apply_pending`] would rediscover the same reorg on every
/// call and spin the caller's tick loop. `(0, 0, reorg_id)` is both the correct
/// initial state and the meaning of a rewind: replay from the beginning with
/// this reorg applied.
pub(super) async fn rewind_consumer(
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
