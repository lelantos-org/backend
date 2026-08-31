use crate::adapters::masp::DynMaspYieldReader;
use crate::error::Result;
use crate::repositories::asset_yield::{self, UpdateState};
use crate::util::u256_to_bigdecimal;
use alloy::primitives::Address;
use database::DbPool;
use shared::tick::TickProgress;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

pub const NAME: &str = "explorer-yield-state";

pub struct YieldStateCtx {
    pub pool: DbPool,
    pub readers: Arc<HashMap<i64, DynMaspYieldReader>>,
}

/// Refresh every yield asset on one chain.
///
/// `batch` is ignored: a chain carries a handful of yield assets, so there is no
/// queue to drain and no partial pass to report. The tick is `Idle` when there
/// is nothing bound and `Partial` once it has written, which lets the driver
/// sleep from the floor rather than spin.
pub async fn tick_chain(ctx: &YieldStateCtx, chain_id: i64, _batch: i64) -> Result<TickProgress> {
    let Some(reader) = ctx.readers.get(&chain_id) else {
        return Ok(TickProgress::Idle);
    };

    let assets = asset_yield::list_for_chain(&ctx.pool, chain_id).await?;
    if assets.is_empty() {
        return Ok(TickProgress::Idle);
    }

    for asset in assets {
        let asset_id = asset.asset_id_u64;
        if let Err(e) = refresh_one(ctx, reader, chain_id, asset).await {
            // One unreachable venue must not stop the others: each asset's row
            // is independent, and a stale row is what the next pass repairs.
            warn!(chain_id, asset_id, error = %e, "yield state refresh failed");
        }
    }

    Ok(TickProgress::Partial)
}

async fn refresh_one(
    ctx: &YieldStateCtx,
    reader: &DynMaspYieldReader,
    chain_id: i64,
    asset: asset_yield::YieldAssetRef,
) -> Result<()> {
    let venue = Address::from_slice(&asset.venue);
    let state = reader.yield_state(venue, asset.asset_id_u64 as u64).await?;

    asset_yield::update_state(
        &ctx.pool,
        chain_id,
        asset.asset_id_u64,
        UpdateState {
            total_normalized: u256_to_bigdecimal(state.total_normalized),
            accrued_fee_normalized: u256_to_bigdecimal(state.accrued_fee_normalized),
            idle: u256_to_bigdecimal(state.idle),
            last_idx: u256_to_bigdecimal(state.last_idx),
            gross: u256_to_bigdecimal(state.gross),
            index_ray: u256_to_bigdecimal(state.index_ray),
            block_number: state.block_number as i64,
        },
    )
    .await?;

    Ok(())
}
