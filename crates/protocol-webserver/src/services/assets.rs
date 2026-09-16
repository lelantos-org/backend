//! The asset catalog, read through the shared registry and shaped for the wire.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{AssetOut, YieldOut};
use crate::services;
use asset_registry::AssetRow;
use std::sync::Arc;

/// Every registered asset, for one chain or for all of them.
///
/// Cached per query rather than read per request: this is what a wallet boots
/// from and polls, and the rows change only when the indexer registers an asset
/// or repolls a venue.
///
/// An unconfigured `chain_id` is a 404 rather than an empty list. Two reasons:
/// an empty list already means "the indexer has not caught up", so overloading
/// it with "no such chain" would leave a client unable to tell a cold start from
/// a typo; and it bounds the cache to chains this deployment actually serves,
/// which an arbitrary caller-supplied key would not.
pub async fn list(st: &AppState, chain_id: Option<i64>) -> AppResult<Arc<Vec<AssetOut>>> {
    if let Some(id) = chain_id
        && !st.serves_chain(id)
    {
        return Err(AppError::NotFound(format!("chain {id}")));
    }
    let cache = st.cache.assets.clone();
    let st = st.clone();
    services::cached(&cache, chain_id, async move {
        let chains = match chain_id {
            Some(c) => vec![c],
            None => st.chain_ids(),
        };
        let rows = asset_registry::list_for_chains(&st.pool, &chains).await?;
        let out: Vec<AssetOut> = rows
            .into_iter()
            .map(|(chain_id, row)| to_out(chain_id, &row))
            .collect();
        Ok(Arc::new(out))
    })
    .await
}

fn to_out(chain_id: i64, a: &AssetRow) -> AssetOut {
    AssetOut {
        chain_id,
        asset_id: a.asset_id_u64,
        token: format!("0x{}", hex::encode(&a.token)),
        scale: a.scale.to_string(),
        decimals: a.decimals,
        symbol: a.symbol.clone(),
        deposit_bps: a.deposit_bps,
        withdraw_bps: a.withdraw_bps,
        yield_state: yield_out(a),
    }
}

/// `None` for a plain asset, and also for a yield asset the poller has not
/// reached yet — a client must not price the latter at `scale`.
fn yield_out(a: &AssetRow) -> Option<YieldOut> {
    let venue = a.venue.as_ref()?;
    let gross = a.gross.as_ref()?;
    let total = a.total_normalized.as_ref()?;
    let fee_units = a.accrued_fee_normalized.as_ref()?;
    // A halted venue is no longer supplied, so whatever it last paid is not a
    // rate this asset earns — dropped rather than published as stale. Read
    // through `AssetRow::apy`, which also drops a measurement that has stopped
    // being refreshed.
    let apy = a.apy().filter(|_| !a.halted.unwrap_or(false));
    Some(YieldOut {
        venue: format!("0x{}", hex::encode(venue)),
        gross: gross.to_string(),
        supply: (total + fee_units).to_string(),
        index: a.index_ray.as_ref()?.to_string(),
        halted: a.halted.unwrap_or(false),
        apy_bps: apy.map(|e| e.bps),
        apy_window_s: apy.map(|e| e.window_s),
        vault_name: a.vault_name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// A yield asset the indexer has polled, carrying a fresh measurement.
    fn yielding() -> AssetRow {
        AssetRow {
            asset_id_u64: 1,
            token: vec![0xAA; 20],
            scale: "1000000000000".parse().expect("scale"),
            decimals: Some(18),
            symbol: Some("mDAI".into()),
            deposit_bps: Some(10),
            withdraw_bps: Some(20),
            venue: Some(vec![0xBB; 20]),
            gross: Some("1100000".parse().expect("gross")),
            total_normalized: Some("1000".parse().expect("total")),
            accrued_fee_normalized: Some("0".parse().expect("fee")),
            halted: Some(false),
            index_ray: Some("1100000000000000000000000000".parse().expect("index")),
            perf_bps: Some(1000),
            buffer_bps: Some(500),
            apy_bps: Some(512),
            apy_window_s: Some(7 * 24 * 60 * 60),
            apy_measured_at: Some(Utc::now()),
            vault_name: Some("Steakhouse USDC".into()),
        }
    }

    /// The vault label rides along as `vaultName`, and is omitted rather than
    /// `null` before the indexer has read it.
    #[test]
    fn test_vault_name_is_published_when_known() {
        let json = serde_json::to_value(yield_out(&yielding()).expect("state")).expect("json");
        assert_eq!(json["vaultName"], "Steakhouse USDC");

        let mut row = yielding();
        row.vault_name = None;
        let json = serde_json::to_value(yield_out(&row).expect("state")).expect("json");
        assert!(json.get("vaultName").is_none());
    }

    #[test]
    fn test_yield_asset_publishes_its_rate() {
        let y = yield_out(&yielding()).expect("a polled yield asset has state");
        assert_eq!(y.apy_bps, Some(512));
        assert_eq!(y.apy_window_s, Some(7 * 24 * 60 * 60));
        assert!(!y.halted);
        // `total + accrued_fee`, the supply the pool converts against.
        assert_eq!(y.supply, "1000");
        assert_eq!(y.gross, "1100000");
    }

    /// A halted venue is no longer supplied, so its last rate is not one this
    /// asset earns. The backing is still real, so the rest of the state stays.
    #[test]
    fn test_halted_venue_publishes_state_but_no_rate() {
        let mut row = yielding();
        row.halted = Some(true);
        let y = yield_out(&row).expect("halted is still fully backed");
        assert!(y.halted);
        assert_eq!(y.apy_bps, None, "a halted venue must not publish a rate");
        assert_eq!(y.apy_window_s, None);
        assert_eq!(y.gross, "1100000", "backing is unaffected by the halt");
    }

    /// An estimate nobody has refreshed stops being published; `AssetRow::apy`
    /// owns that rule and this is the path that must honour it.
    #[test]
    fn test_stale_measurement_is_not_published() {
        let mut row = yielding();
        row.apy_measured_at = Some(Utc::now() - chrono::Duration::days(1));
        let y = yield_out(&row).expect("state is still known");
        assert_eq!(y.apy_bps, None);
    }

    /// A plain asset prices at `scale` forever and has no venue to report.
    #[test]
    fn test_plain_asset_has_no_yield_state() {
        let mut row = yielding();
        row.venue = None;
        assert!(yield_out(&row).is_none());
    }

    /// Known to be yield-bearing but not yet polled. Publishing partial state
    /// would invite a client to price it at `scale`, which is wrong by whatever
    /// the venue has earned.
    #[test]
    fn test_unpolled_yield_asset_reports_nothing_rather_than_partial_state() {
        for blank in [0, 1, 2] {
            let mut row = yielding();
            match blank {
                0 => row.gross = None,
                1 => row.total_normalized = None,
                _ => row.index_ray = None,
            }
            assert!(
                yield_out(&row).is_none(),
                "a half-polled row must not be published"
            );
        }
    }

    #[test]
    fn test_addresses_are_rendered_hex_prefixed() {
        let out = to_out(31337, &yielding());
        assert_eq!(out.token, format!("0x{}", "aa".repeat(20)));
        assert_eq!(
            out.yield_state.expect("venue").venue,
            format!("0x{}", "bb".repeat(20))
        );
        assert_eq!(out.chain_id, 31337);
    }
}
