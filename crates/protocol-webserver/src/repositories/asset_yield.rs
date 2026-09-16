//! The rate estimate's write path.
//!
//! `asset_yield` is otherwise protocol-indexer's table: it creates the row from
//! `YieldAssetAdded` and polls the state columns. These three are the exception,
//! written by whichever replica holds this chain's measurement lock — see
//! `handlers::worker::venue_apy`.
//!
//! An `UPDATE` rather than an upsert. An estimate only exists for an asset that
//! is already bound to a venue, and that binding is what created the row, so a
//! miss here means the asset is not yield-bearing — nothing to record, and
//! inventing a row would fabricate a binding the chain never made.

use crate::domain::error::AppResult;
use asset_registry::ApyEstimate;
use chrono::Utc;
use database::schema::asset_yield;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Store one asset's estimate. Returns whether a row was updated.
///
/// `measured_at` is stamped here rather than taken from the caller: it is what
/// readers age the figure against, so it must mean "when this was computed" and
/// nothing else.
pub async fn store_estimate(
    pool: &database::DbPool,
    chain_id: i64,
    asset_id_u64: i64,
    est: ApyEstimate,
) -> AppResult<bool> {
    let mut conn = super::conn(pool).await?;
    let n = diesel::update(asset_yield::table.find((chain_id, asset_id_u64)))
        .set((
            asset_yield::apy_bps.eq(est.bps),
            asset_yield::apy_window_s.eq(est.window_s),
            asset_yield::apy_measured_at.eq(Utc::now()),
        ))
        .execute(&mut conn)
        .await
        .map_err(super::db_err)?;
    Ok(n > 0)
}
