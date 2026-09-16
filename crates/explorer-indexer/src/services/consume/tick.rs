//! One chain's consume tick: reorg, fetch, decode into a plan, write, advance.

use super::events::plan_event;
use super::plan::CommitPlan;
use super::refresh::{RefreshGate, View};
use crate::adapters::ChainLocks;
use crate::domain::error::Result;
use chain_types::decode;
use database::raw_events;
use database::reorg::Owner;
use database::{CursorRepo, DbPool, PostgresCursorRepo, UpsertCursor};
use shared::entities::{Consumer, EventKind};
use shared::tick::TickProgress;
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::{debug, warn};

/// This binary's owner: names both its cursor row and the tables a reorg
/// retracts for it. One constant so the two cannot drift apart.
pub const OWNER: Owner = Owner::Explorer;
pub const NAME: &str = OWNER.cursor_name();

/// The kinds this service fetches, as the `ANY` array wants them.
///
/// Derived from [`EventKind::kinds_for`] rather than restated here. That
/// partition is one exhaustive match with no wildcard arm, so a new variant
/// fails to compile until it is assigned to a consumer — which is what stops a
/// kind being handled in `plan_event` but missing from the fetch. That exact
/// drift once left `asset_yield` permanently empty, because the cursor only
/// advances to the highest id among the kinds actually fetched.
fn kinds() -> &'static [i16] {
    static KINDS: OnceLock<Vec<i16>> = OnceLock::new();
    KINDS.get_or_init(|| {
        EventKind::kinds_for(Consumer::Explorer)
            .into_iter()
            .map(EventKind::as_i16)
            .collect()
    })
}

pub struct ConsumeCtx {
    pub pool: DbPool,
    /// Decides when the explorer's materialized views are rebuilt. Shared across
    /// chains, since the views are not per-chain.
    pub refresh: Arc<RefreshGate>,
    /// Per-chain leadership, checked at the top of every tick.
    pub locks: Arc<ChainLocks>,
}

/// Consume one window of `raw_events` for `chain_id`.
///
/// Retracts any pending reorg, decodes the whole window into a `CommitPlan`,
/// applies it in one batched call per projection, gates the materialized-view
/// rebuild, and only then advances the cursor — so a crash anywhere between the
/// fetch and the commit replays the same window over idempotent writes.
pub async fn tick_chain(ctx: &ConsumeCtx, chain_id: i64, batch: i64) -> Result<TickProgress> {
    // Leadership first, before any read. A standby that fetched the window
    // anyway would duplicate the whole tick: the same rows written twice and,
    // more expensively, the same whole-table view rebuild run twice. `Idle` so
    // the driver backs off to its idle ceiling rather than spinning.
    if !ctx.locks.is_leader(chain_id).await? {
        return Ok(TickProgress::Idle);
    }

    let cursors = PostgresCursorRepo::new(ctx.pool.clone());

    // Retract before reading. Replacement rows for a reorged range come back
    // with fresh, higher ids and replay on their own, but the stats and ledger
    // rows derived from the deleted rows sit below the cursor where nothing
    // revisits them. Applying the reorg log first drops those and rewinds the
    // cursor so the replay rebuilds them, which is queued work.
    if database::reorg::apply_pending(&ctx.pool, OWNER, chain_id).await? > 0 {
        return Ok(TickProgress::Saturated);
    }

    let (after, _) = cursors.fetch(NAME, chain_id).await?;
    let max_id = raw_events::max_id(&ctx.pool, chain_id).await?;
    if after > max_id {
        warn!(chain_id, "cursor ahead; reset");
        reset_cursor(&cursors, chain_id).await?;
        return Ok(TickProgress::Saturated);
    }

    let rows = raw_events::batch_after(&ctx.pool, chain_id, after, kinds(), batch).await?;
    if rows.is_empty() {
        return Ok(TickProgress::Idle);
    }

    // A full batch means more rows are already queued behind it.
    let progress = TickProgress::from_batch(rows.len(), batch);

    let mut last_id = after;
    let mut last_block = 0i64;

    // Decode first, write after. Nothing in this loop touches the database, so
    // the pool is untouched until the window is fully planned; see `CommitPlan`.
    let mut plan = CommitPlan::default();
    for row in &rows {
        let Some(kind) = EventKind::from_i16(row.event_kind) else {
            continue;
        };
        match decode::decode(kind, &row.topics, &row.data) {
            Ok(events) => {
                for event in events {
                    plan_event(&mut plan, chain_id, row, event);
                }
            }
            // A log that will not decode never will; the cursor still advances
            // past it rather than wedging the chain on one bad row.
            Err(e) => warn!(chain_id, id = row.id, error = %e, "decode failed; skipped"),
        }
        last_id = row.id;
        last_block = row.block_number;
    }

    // Before the cursor moves, and the only fallible step between decoding and
    // committing: a failure here leaves the cursor where it was, and the next
    // tick replays the same window over idempotent writes.
    plan.apply(&ctx.pool).await?;

    // Marked, then flushed at most once per group: the rebuild is a whole-table
    // aggregate, so it is gated on catching up rather than run inline. See
    // `RefreshGate`.
    if plan.touched_flows() {
        ctx.refresh.mark(View::AssetFlows).await;
    }
    ctx.refresh
        .flush(&ctx.pool, progress != TickProgress::Saturated)
        .await;

    advance_cursor(&cursors, chain_id, last_id, last_block).await?;
    debug!(chain_id, processed = rows.len(), last_id, "explorer commit");
    Ok(progress)
}

/// This consumer's cursor row. One spelling of `NAME`, so the advance and the
/// reset cannot drift.
fn cursor_row(chain_id: i64, last_event_id: i64, last_block_number: i64) -> UpsertCursor {
    UpsertCursor {
        name: NAME.to_string(),
        chain_id,
        last_event_id,
        last_block_number,
    }
}

/// Move the cursor past the window just committed.
///
/// `upsert_monotonic`, per the repo convention: a plain `upsert` would let a
/// stale watermark overwrite a further one and replay an unbounded range. The
/// per-chain lock already makes this tick the only writer, so the guard is the
/// backstop for a split brain rather than the thing preventing one. The
/// returned flag is dropped: a refused advance means a further cursor already
/// stands, which is the outcome this call wanted.
async fn advance_cursor(
    cursors: &PostgresCursorRepo,
    chain_id: i64,
    last_event_id: i64,
    last_block_number: i64,
) -> Result<()> {
    cursors
        .upsert_monotonic(cursor_row(chain_id, last_event_id, last_block_number))
        .await?;
    Ok(())
}

/// Rewind to the start of the chain, for a cursor found ahead of `raw_events`.
///
/// The one deliberate backwards move in this crate, so the one place that may
/// use the unguarded `upsert`: `upsert_monotonic` would refuse it.
async fn reset_cursor(cursors: &PostgresCursorRepo, chain_id: i64) -> Result<()> {
    cursors.upsert(cursor_row(chain_id, 0, 0)).await?;
    Ok(())
}
