use super::events;
use crate::config::ExplorerIndexerConfig;
use crate::error::{ExplorerIndexerError, Result};
use crate::repositories::{asset_flows, raw_events, tree_advances};
use chain_types::decode::{self, DecodedEvent};
use database::{CursorRepo, DbPool, PostgresCursorRepo, UpsertCursor};
use shared::entities::EventKind;
use std::sync::Arc;
use tracing::{debug, warn};

pub const NAME: &str = "explorer";

/// Public stats + intent-escrow ledger. NoteCreated is FMD-zone
/// (fmd-indexer consumes it). NullifierConsumed lives there too.
const KINDS: [i16; 6] = [
    EventKind::AssetRegistered as i16,
    EventKind::RootAdvanced as i16,
    EventKind::AssetMoved as i16,
    EventKind::IntentEscrowed as i16,
    EventKind::IntentFlushed as i16,
    EventKind::IntentCanceled as i16,
];

pub struct ConsumeCtx {
    pub pool: DbPool,
    pub cfg: Arc<ExplorerIndexerConfig>,
}

pub async fn tick_chain(ctx: &ConsumeCtx, chain_id: i64, batch: i64) -> Result<()> {
    let cursors = PostgresCursorRepo::new(ctx.pool.clone());
    let (after, _) = cursors.fetch(NAME, chain_id).await?;
    let max_id = raw_events::max_id(&ctx.pool, chain_id).await?;
    if after > max_id {
        warn!(chain_id, "cursor ahead; reset");
        return reset_cursor(&cursors, chain_id).await;
    }

    let rows = raw_events::batch_after(&ctx.pool, chain_id, after, &KINDS, batch).await?;
    if rows.is_empty() {
        return Ok(());
    }

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

    if root_advanced_seen && let Err(e) = tree_advances::refresh_hourly_mv(&ctx.pool).await {
        warn!(chain_id, "tree_advances_hourly refresh failed: {}", e);
    }
    if asset_moved_seen && let Err(e) = asset_flows::refresh_hourly_mv(&ctx.pool).await {
        warn!(chain_id, "asset_flows_hourly refresh failed: {}", e);
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
    Ok(())
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
        DecodedEvent::IntentEscrowed {
            id,
            payer,
            recipient,
            public_asset_id,
            public_in,
            fee_bps_at_submit,
            cm0,
            cm1,
            cv_dep0_x,
            cv_dep0_y,
            cv_dep1_x,
            cv_dep1_y,
            rcv_total,
            clue_rx0,
            clue_ry0,
            eph_pub_x0,
            eph_pub_y0,
            ciphertext0,
            clue_rx1,
            clue_ry1,
            eph_pub_x1,
            eph_pub_y1,
            ciphertext1,
        } => {
            let aux = events::encode_aux(
                clue_rx0,
                clue_ry0,
                eph_pub_x0,
                eph_pub_y0,
                &ciphertext0,
                clue_rx1,
                clue_ry1,
                eph_pub_x1,
                eph_pub_y1,
                &ciphertext1,
            );
            events::intent_escrowed(
                &ctx.pool,
                chain_id,
                row,
                id,
                payer,
                recipient,
                public_asset_id,
                public_in,
                fee_bps_at_submit,
                cm0,
                cm1,
                cv_dep0_x,
                cv_dep0_y,
                cv_dep1_x,
                cv_dep1_y,
                rcv_total,
                aux,
            )
            .await
        }
        DecodedEvent::IntentFlushed { id, .. } => {
            events::intent_flushed(&ctx.pool, chain_id, row, id).await
        }
        DecodedEvent::IntentCanceled { id, .. } => {
            events::intent_canceled(&ctx.pool, chain_id, row, id).await
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
