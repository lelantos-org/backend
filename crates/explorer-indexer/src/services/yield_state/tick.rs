use crate::adapters::masp::{DynMaspYieldReader, YieldState};
use crate::error::Result;
use crate::repositories::asset_yield::{self, UpdateState};
use crate::util::u256_to_bigdecimal;
use alloy::primitives::Address;
use database::DbPool;
use shared::tick::TickProgress;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;

pub const NAME: &str = "explorer-yield-state";

/// How stale `asset_yield.updated_at` may become while nothing about an asset
/// moves.
///
/// The row is rewritten on every pass otherwise, and on an idle chain nothing in
/// it changes — so at the configured cadence that is a no-op `UPDATE` per asset
/// several times a second, forever, each one a fresh heap tuple plus WAL plus
/// index churn for a byte-identical row.
///
/// Skipping the write outright is not available: `updated_at` is public API,
/// documented as when the values were last *confirmed* against the chain, so a
/// consumer ageing it to spot a stalled indexer would see every healthy idle
/// asset as stale. The heartbeat keeps that reading true to within this
/// interval while removing the rest of the writes.
const FRESHNESS_HEARTBEAT: Duration = Duration::from_secs(30);

pub struct YieldStateCtx {
    pub pool: DbPool,
    pub readers: Arc<HashMap<i64, DynMaspYieldReader>>,
    /// Last state written per `(chain, asset)`, and when. Drives the
    /// unchanged-row skip; see [`FRESHNESS_HEARTBEAT`].
    ///
    /// In-process rather than a `WHERE … IS DISTINCT FROM` on the update: one
    /// indexer owns these rows, and a cache miss costs one redundant write
    /// rather than a wrong answer.
    written: Mutex<HashMap<(i64, i64), (UpdateState, Instant)>>,
}

impl YieldStateCtx {
    pub fn new(pool: DbPool, readers: Arc<HashMap<i64, DynMaspYieldReader>>) -> Self {
        Self {
            pool,
            readers,
            written: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `next` has to reach the database.
    ///
    /// True when anything in the row moved, or when the last write is old enough
    /// that `updated_at` would start to read as stale.
    ///
    /// Read-only: the cache is updated by {@link Self::record_written} *after*
    /// the write lands. Recording it here would let a failed `UPDATE` mark the
    /// state durable, and the next pass — finding nothing changed — would skip
    /// the repair for a full heartbeat while `updated_at` claimed the row was
    /// confirmed.
    fn needs_write(&self, key: (i64, i64), next: &UpdateState) -> bool {
        /// Everything except `block_number`.
        ///
        /// The head moves on every block, so comparing it would make every pass
        /// on a live chain look changed and the skip would fire only on an idle
        /// instant-mine anvil — that is, only where this was first measured. What
        /// the row is *about* is the venue's accounting; the block is the stamp
        /// on it, and re-stamping an otherwise identical row is exactly the
        /// no-op write being avoided.
        fn values(s: &UpdateState) -> impl PartialEq + '_ {
            (
                &s.total_normalized,
                &s.accrued_fee_normalized,
                &s.idle,
                &s.last_idx,
                &s.gross,
                &s.index_ray,
            )
        }

        // `into_inner` rather than `expect`: the cache is advisory, and a
        // poisoned lock would otherwise panic every tick — which, since a panic
        // in a tick kills the worker silently, would end the service.
        let written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        !matches!(
            written.get(&key),
            Some((prev, at)) if values(prev) == values(next) && at.elapsed() < FRESHNESS_HEARTBEAT
        )
    }

    /// Note that `state` is now the row's durable content.
    fn record_written(&self, key: (i64, i64), state: UpdateState) {
        self.written
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, (state, Instant::now()));
    }
}

/// Refresh every yield asset on one chain.
///
/// `batch` is ignored: a chain carries a handful of yield assets, so there is no
/// queue to drain and no partial pass to report.
///
/// `Polled` on a pass that read the chain, `Idle` only when there was nothing to
/// read. This service has no cursor and no queue — it re-reads the same handful
/// of assets forever — so the cadence is the answer rather than something the
/// backoff should search for.
///
/// Reporting `Partial` here — "the queue is drained, sleep from the floor" —
/// reset the backoff on every pass, and the floor is `Backoff::IDLE_MIN`, 50 ms.
/// That pinned the service at ~20 rounds/second against the node instead of the
/// configured `tick_ms`: with three bound assets it was ~180 `eth_call` and 60
/// `eth_blockNumber` per second, forever, on an idle chain.
///
/// `Idle` would fix the cadence too, but it is the driver's word for "nothing
/// happened", and `TickProgress::label` is a metric dimension — so a service
/// reporting `idle` on every round could no longer be told apart from one with
/// no assets bound or no reader configured. That is the exact shape of the bug
/// that left `asset_yield` permanently empty, so the two states stay distinct.
pub async fn tick_chain(ctx: &YieldStateCtx, chain_id: i64, _batch: i64) -> Result<TickProgress> {
    let Some(reader) = ctx.readers.get(&chain_id) else {
        return Ok(TickProgress::Idle);
    };

    let assets = asset_yield::list_for_chain(&ctx.pool, chain_id).await?;
    if assets.is_empty() {
        return Ok(TickProgress::Idle);
    }

    // The whole round in one call where the chain has Multicall3, and one pair
    // of reads per asset where it does not. Either way every state is stamped
    // with a single head, so the rows written together describe one block.
    let round = reader
        .round(
            &assets
                .iter()
                .map(|a| (Address::from_slice(&a.venue), a.asset_id_u64 as u64))
                .collect::<Vec<_>>(),
        )
        .await?;

    for (asset, state) in assets.iter().zip(round) {
        // `None` is one asset's reads failing, logged by the reader where it
        // happened. The rest of the round still lands: each row is independent,
        // and a stale one is what the next pass repairs.
        let Some(state) = state else { continue };
        if let Err(e) = write_one(ctx, chain_id, asset.asset_id_u64, &state).await {
            warn!(chain_id, asset_id = asset.asset_id_u64, error = %e, "yield state write failed");
        }
    }

    Ok(TickProgress::Polled)
}

async fn write_one(
    ctx: &YieldStateCtx,
    chain_id: i64,
    asset_id: i64,
    state: &YieldState,
) -> Result<()> {
    let next = UpdateState {
        total_normalized: u256_to_bigdecimal(state.total_normalized),
        accrued_fee_normalized: u256_to_bigdecimal(state.accrued_fee_normalized),
        idle: u256_to_bigdecimal(state.idle),
        last_idx: u256_to_bigdecimal(state.last_idx),
        gross: u256_to_bigdecimal(state.gross),
        index_ray: u256_to_bigdecimal(state.index_ray),
        block_number: state.block_number as i64,
    };

    // The read still happens every pass — that is the point of polling — but an
    // unchanged row does not have to be rewritten to say so.
    if ctx.needs_write((chain_id, asset_id), &next) {
        asset_yield::update_state(&ctx.pool, chain_id, asset_id, next.clone()).await?;
        ctx.record_written((chain_id, asset_id), next);
    }

    Ok(())
}
