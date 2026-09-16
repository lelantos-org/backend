//! Database reads for the asset catalog.

use crate::error::{Error, Result};
use crate::row::AssetRow;
use database::schema::{asset_yield, assets};
use database::{DbConn, DbPool};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// Check out a pooled connection, mapping exhaustion or a dead pool to
/// [`Error::Db`].
async fn conn(pool: &DbPool) -> Result<DbConn<'_>> {
    pool.get().await.map_err(|e| Error::Db(e.to_string()))
}

/// Every registered asset on `chain_id`, lowest id first.
pub async fn list_for_chain(pool: &DbPool, chain_id: i64) -> Result<Vec<AssetRow>> {
    let rows = list_keyed(pool, Some(&[chain_id])).await?;
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

/// Every registered asset on each of `chain_ids`, ordered by `(chain_id,
/// asset_id_u64)`.
///
/// One statement and one pooled connection for the whole set. Calling
/// [`list_for_chain`] per chain costs a checkout each, and the relayer pool has
/// four connections.
pub async fn list_for_chains(pool: &DbPool, chain_ids: &[i64]) -> Result<Vec<(i64, AssetRow)>> {
    if chain_ids.is_empty() {
        return Ok(Vec::new());
    }
    list_keyed(pool, Some(chain_ids)).await
}

/// Every registered asset on every indexed chain, ordered by `(chain_id,
/// asset_id_u64)`.
///
/// For a caller that serves the whole catalog and does not already hold the
/// chain list: reading `chain_state` first and passing it to [`list_for_chains`]
/// would cost a second round trip to learn something this query does not need.
pub async fn list_all(pool: &DbPool) -> Result<Vec<(i64, AssetRow)>> {
    list_keyed(pool, None).await
}

/// The `assets ⋈ asset_yield` read, optionally narrowed to a set of chains.
///
/// `None` means every chain. The column list is written once here because the
/// two public forms differ only in their filter, and a `select` that drifted
/// between them would silently return a different row shape.
async fn list_keyed(pool: &DbPool, chain_ids: Option<&[i64]>) -> Result<Vec<(i64, AssetRow)>> {
    let mut conn = conn(pool).await?;
    let mut q = assets::table
        .left_join(
            asset_yield::table.on(asset_yield::chain_id
                .eq(assets::chain_id)
                .and(asset_yield::asset_id_u64.eq(assets::asset_id_u64))),
        )
        .into_boxed();
    if let Some(ids) = chain_ids {
        q = q.filter(assets::chain_id.eq_any(ids.to_vec()));
    }
    q.order((assets::chain_id.asc(), assets::asset_id_u64.asc()))
        .select((
            assets::chain_id,
            (
                assets::asset_id_u64,
                assets::token,
                assets::scale,
                assets::decimals,
                assets::symbol,
                assets::deposit_bps,
                assets::withdraw_bps,
                asset_yield::venue.nullable(),
                asset_yield::gross.nullable(),
                asset_yield::total_normalized.nullable(),
                asset_yield::accrued_fee_normalized.nullable(),
                asset_yield::halted.nullable(),
                asset_yield::index_ray.nullable(),
                asset_yield::perf_bps.nullable(),
                asset_yield::buffer_bps.nullable(),
                asset_yield::apy_bps.nullable(),
                asset_yield::apy_window_s.nullable(),
                asset_yield::apy_measured_at.nullable(),
                asset_yield::vault_name.nullable(),
            ),
        ))
        .load::<(i64, AssetRow)>(&mut conn)
        .await
        .map_err(|e| Error::Db(e.to_string()))
}
