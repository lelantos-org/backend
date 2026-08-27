use super::events;
use crate::adapters::DynTokenMetadata;
use crate::config::ExplorerIndexerConfig;
use crate::error::{ExplorerIndexerError, Result};
use crate::repositories::{asset_flows, assets, raw_events, tree_advances};
use alloy::primitives::Address;
use chain_types::decode::{self, DecodedEvent};
use database::{CursorRepo, DbPool, PostgresCursorRepo, UpsertCursor};
use shared::entities::EventKind;
use shared::tick::TickProgress;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

pub const NAME: &str = "explorer";

/// Kinds backing the public stats and the deposit-escrow ledger. `NoteCreated`
/// and `NullifierConsumed` belong to the FMD zone and are consumed by
/// fmd-indexer.
const KINDS: [i16; 7] = [
    EventKind::AssetRegistered as i16,
    EventKind::AssetFeeSet as i16,
    EventKind::RootAdvanced as i16,
    EventKind::AssetMoved as i16,
    EventKind::DepositEscrowed as i16,
    EventKind::DepositFlushed as i16,
    EventKind::DepositCanceled as i16,
];

pub struct ConsumeCtx {
    pub pool: DbPool,
    pub cfg: Arc<ExplorerIndexerConfig>,
    /// Per-chain ERC20 metadata reader. Chains without one keep
    /// `assets.decimals = NULL`.
    pub token_meta: Arc<HashMap<i64, DynTokenMetadata>>,
}

/// How many assets one tick tries to resolve. The registry is small and grows
/// only on `AssetRegistered`, so this throttles retry storms while an RPC is
/// down rather than serving as a paging mechanism.
const METADATA_PER_TICK: i64 = 16;

/// Fill in `decimals` and `symbol` for assets that do not have them yet.
///
/// Runs outside the event path: `AssetRegistered` carries neither column, and
/// inline RPC reads would let a flaky endpoint stall event consumption or drop
/// the values permanently. Sweeping retries a failed read on the next tick and
/// repairs rows that predate these columns.
///
/// Each column is fetched only when absent and written only when resolved, so a
/// token whose `symbol()` reverts still gets its decimals, and neither read can
/// clear the other's stored value.
/// Resolve `fut` only when `wanted`, so both calls can sit in one `join!`.
///
/// Building the future does not start the call; nothing is sent for the arm that
/// is not wanted.
async fn fetch_if<T>(wanted: bool, fut: impl std::future::Future<Output = T>) -> Option<T> {
    if wanted { Some(fut.await) } else { None }
}

async fn fill_missing_metadata(ctx: &ConsumeCtx, chain_id: i64) {
    let Some(rpc) = ctx.token_meta.get(&chain_id) else {
        return;
    };
    let pending = match assets::missing_metadata(&ctx.pool, chain_id, METADATA_PER_TICK).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(chain_id, "metadata backfill query failed: {}", e);
            return;
        }
    };
    for row in pending {
        let asset_id_u64 = row.asset_id_u64;
        let Ok(bytes) = <[u8; 20]>::try_from(row.token.as_slice()) else {
            warn!(chain_id, asset_id_u64, "token is not a 20-byte address");
            continue;
        };
        let token = Address::from(bytes);
        let mut meta = assets::AssetMetadata::default();

        // Two independent `eth_call`s, issued together rather than one round trip
        // after the other. `None` is a column that already has a value.
        let (decimals, symbol) = tokio::join!(
            fetch_if(row.decimals.is_none(), rpc.decimals(token)),
            fetch_if(row.symbol.is_none(), rpc.symbol(token)),
        );
        match decimals {
            Some(Ok(d)) => meta.decimals = Some(i16::from(d)),
            // Left NULL and retried next tick rather than defaulted; assuming 18
            // would misreport every amount.
            Some(Err(e)) => warn!(chain_id, asset_id_u64, "decimals() failed: {}", e),
            None => {}
        }
        match symbol {
            Some(Ok(s)) => meta.symbol = Some(s),
            Some(Err(e)) => warn!(chain_id, asset_id_u64, "symbol() failed: {}", e),
            None => {}
        }

        if meta.is_empty() {
            continue;
        }
        if let Err(e) = assets::set_metadata(&ctx.pool, chain_id, asset_id_u64, meta).await {
            warn!(
                chain_id,
                asset_id_u64, "storing asset metadata failed: {}", e
            );
        } else {
            debug!(chain_id, asset_id_u64, "asset metadata resolved");
        }
    }
}

pub async fn tick_chain(ctx: &ConsumeCtx, chain_id: i64, batch: i64) -> Result<TickProgress> {
    let cursors = PostgresCursorRepo::new(ctx.pool.clone());

    // Retract before reading. Replacement rows for a reorged range come back
    // with fresh, higher ids and replay on their own, but the stats and ledger
    // rows derived from the deleted rows sit below the cursor where nothing
    // revisits them. Applying the reorg log first drops those and rewinds the
    // cursor so the replay rebuilds them, which is queued work.
    if database::reorg::apply_pending(&ctx.pool, NAME, chain_id).await? > 0 {
        return Ok(TickProgress::Saturated);
    }

    let (after, _) = cursors.fetch(NAME, chain_id).await?;
    let max_id = raw_events::max_id(&ctx.pool, chain_id).await?;
    if after > max_id {
        warn!(chain_id, "cursor ahead; reset");
        reset_cursor(&cursors, chain_id).await?;
        return Ok(TickProgress::Saturated);
    }

    let rows = raw_events::batch_after(&ctx.pool, chain_id, after, &KINDS, batch).await?;
    if rows.is_empty() {
        // Sweep anyway: a previous attempt may have failed, and an idle chain
        // has room to retry.
        fill_missing_metadata(ctx, chain_id).await;
        return Ok(TickProgress::Idle);
    }

    // A full batch means more rows are already queued behind it.
    let progress = TickProgress::from_batch(rows.len(), batch);

    let mut last_id = after;
    let mut last_block = 0i64;
    let mut root_advanced_seen = false;
    let mut asset_moved_seen = false;

    for row in &rows {
        let Some(kind) = EventKind::from_i16(row.event_kind) else {
            continue;
        };
        if let Ok(events) = decode::decode(kind, &row.topics, &row.data) {
            for event in events {
                match &event {
                    DecodedEvent::RootAdvanced { .. } => root_advanced_seen = true,
                    DecodedEvent::AssetMoved { .. } => asset_moved_seen = true,
                    _ => {}
                }
                dispatch(ctx, chain_id, row, event).await?;
            }
        }
        last_id = row.id;
        last_block = row.block_number;
    }

    fill_missing_metadata(ctx, chain_id).await;

    if root_advanced_seen && let Err(e) = tree_advances::refresh_hourly_mv(&ctx.pool).await {
        warn!(chain_id, "tree_advances_hourly refresh failed: {}", e);
    }
    if asset_moved_seen {
        // Both views derive from the rows this tick wrote. Attempted and logged
        // independently so a failure on one does not leave the other stale.
        if let Err(e) = asset_flows::refresh_hourly_mv(&ctx.pool).await {
            warn!(chain_id, "asset_flows_hourly refresh failed: {}", e);
        }
        if let Err(e) = asset_flows::refresh_locked_mv(&ctx.pool).await {
            warn!(chain_id, "asset_locked refresh failed: {}", e);
        }
    }

    cursors
        .upsert(UpsertCursor {
            name: NAME.to_string(),
            chain_id,
            last_event_id: last_id,
            last_block_number: last_block,
        })
        .await?;
    debug!(chain_id, processed = rows.len(), last_id, "explorer commit");
    Ok(progress)
}

async fn dispatch(
    ctx: &ConsumeCtx,
    chain_id: i64,
    row: &raw_events::RawEventRow,
    event: DecodedEvent,
) -> std::result::Result<(), ExplorerIndexerError> {
    match event {
        DecodedEvent::AssetRegistered {
            asset_id,
            token,
            scale,
        } => events::asset_registered(&ctx.pool, chain_id, asset_id, token, scale).await,
        DecodedEvent::AssetFeeSet {
            asset_id,
            deposit_bps,
            withdraw_bps,
        } => events::asset_fee_set(&ctx.pool, chain_id, asset_id, deposit_bps, withdraw_bps).await,
        DecodedEvent::RootAdvanced {
            start_index,
            inserted,
            old_root,
            new_root,
        } => {
            events::root_advanced(
                &ctx.pool,
                chain_id,
                row,
                start_index,
                inserted,
                old_root,
                new_root,
            )
            .await
        }
        DecodedEvent::AssetMoved {
            asset_id,
            token,
            in_amount,
            out_amount,
        } => {
            events::asset_moved(
                &ctx.pool, chain_id, row, asset_id, token, in_amount, out_amount,
            )
            .await
        }
        DecodedEvent::NoteCreated { .. } => Ok(()),
        DecodedEvent::NullifierConsumed { .. } => Ok(()),
        DecodedEvent::DepositEscrowed {
            id,
            payer,
            recipient,
            public_asset_id,
            public_in,
            fee_bps_at_submit,
            cm,
            cv_dep_x,
            cv_dep_y,
            rcv,
            clue_rx,
            clue_ry,
            eph_pub_x,
            eph_pub_y,
            ciphertext,
            fee,
        } => {
            let aux = events::encode_aux(clue_rx, clue_ry, eph_pub_x, eph_pub_y, &ciphertext);
            events::deposit_escrowed(
                &ctx.pool,
                chain_id,
                row,
                id,
                payer,
                recipient,
                public_asset_id,
                public_in,
                fee_bps_at_submit,
                cm,
                cv_dep_x,
                cv_dep_y,
                rcv,
                aux,
                fee,
            )
            .await
        }
        DecodedEvent::DepositFlushed { id, .. } => {
            events::deposit_flushed(&ctx.pool, chain_id, row, id).await
        }
        DecodedEvent::DepositCanceled { id, .. } => {
            events::deposit_canceled(&ctx.pool, chain_id, row, id).await
        }
    }
}

async fn reset_cursor(cursors: &PostgresCursorRepo, chain_id: i64) -> Result<()> {
    cursors
        .upsert(UpsertCursor {
            name: NAME.to_string(),
            chain_id,
            last_event_id: 0,
            last_block_number: 0,
        })
        .await?;
    Ok(())
}
