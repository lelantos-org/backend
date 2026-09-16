//! The recorded index history, grouped for the wire.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{YieldIndexAssetOut, YieldIndexResponse, YieldSampleOut};
use crate::repositories::yield_samples;
use crate::services;
use std::sync::Arc;

/// One chain's history.
///
/// Cached per chain like the catalog: every wallet on a chain asks for the same
/// body, and the rows behind it change only when the sampler runs.
///
/// An unconfigured `chain_id` is a 404 rather than an empty body, matching
/// `services::assets::list` — an empty `assets` already means "nothing sampled
/// yet", so overloading it with "no such chain" would leave a client unable to
/// tell a cold start from a typo.
pub async fn get(st: &AppState, chain_id: i64) -> AppResult<Arc<YieldIndexResponse>> {
    if !st.serves_chain(chain_id) {
        return Err(AppError::NotFound(format!("chain {chain_id}")));
    }
    let cache = st.cache.yield_index.clone();
    let st = st.clone();
    services::cached(&cache, chain_id, async move {
        let rows = yield_samples::history(&st.pool, chain_id).await?;
        Ok(Arc::new(group(chain_id, rows)))
    })
    .await
}

/// Fold block-ordered rows into per-asset series.
///
/// Split from the read so the grouping is testable without a database. Relies on
/// the query's `ORDER BY asset_id_u64, block_number`: a run of rows for one
/// asset is contiguous and already ascending, so this is one pass with no sort.
fn group(chain_id: i64, rows: Vec<yield_samples::HistoryRow>) -> YieldIndexResponse {
    let mut assets: Vec<YieldIndexAssetOut> = Vec::new();
    for row in rows {
        let sample = YieldSampleOut {
            block: row.block_number,
            index_ray: row.index_ray.to_string(),
        };
        match assets.last_mut() {
            Some(last) if last.asset_id == row.asset_id_u64 => last.samples.push(sample),
            _ => assets.push(YieldIndexAssetOut {
                asset_id: row.asset_id_u64,
                samples: vec![sample],
            }),
        }
    }
    YieldIndexResponse { chain_id, assets }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::{BigDecimal, FromPrimitive};
    use yield_samples::HistoryRow;

    fn row(asset: i64, block: i64, index: u64) -> HistoryRow {
        HistoryRow {
            asset_id_u64: asset,
            block_number: block,
            index_ray: BigDecimal::from_u64(index).unwrap(),
        }
    }

    #[test]
    fn test_group_contiguous_rows_yields_one_series_per_asset() {
        let out = group(1, vec![row(7, 10, 100), row(7, 20, 200), row(8, 15, 300)]);

        assert_eq!(out.chain_id, 1);
        assert_eq!(out.assets.len(), 2);
        assert_eq!(out.assets[0].asset_id, 7);
        assert_eq!(out.assets[0].samples.len(), 2);
        assert_eq!(out.assets[1].asset_id, 8);
        assert_eq!(out.assets[1].samples.len(), 1);
    }

    /// Interpolation walks the series assuming ascending blocks, so the order
    /// the query established has to survive the fold.
    #[test]
    fn test_group_preserves_block_order_within_an_asset() {
        let out = group(1, vec![row(7, 10, 100), row(7, 20, 200), row(7, 30, 300)]);

        let blocks: Vec<i64> = out.assets[0].samples.iter().map(|s| s.block).collect();
        assert_eq!(blocks, vec![10, 20, 30]);
    }

    /// A RAY index exceeds what an f64 holds exactly, which is why it travels as
    /// a decimal string.
    #[test]
    fn test_group_renders_an_index_beyond_float_precision_exactly() {
        let big: BigDecimal = "1000000000000000000000000001".parse().unwrap();
        let out = group(
            1,
            vec![HistoryRow {
                asset_id_u64: 7,
                block_number: 10,
                index_ray: big.clone(),
            }],
        );

        assert_eq!(out.assets[0].samples[0].index_ray, big.to_string());
    }

    #[test]
    fn test_group_no_rows_yields_no_assets() {
        assert!(group(1, vec![]).assets.is_empty());
    }
}
