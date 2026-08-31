use crate::app::AppState;
use crate::domain::amount::{plain_amount, whole_tokens_str};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::YieldAssetOut;
use crate::repositories::asset_yield::{self, YieldRow};
use bigdecimal::num_bigint::ToBigInt;
use bigdecimal::{BigDecimal, Zero};
use std::sync::Arc;

pub async fn list(st: &AppState, chain_id: Option<i64>) -> AppResult<Arc<Vec<YieldAssetOut>>> {
    let cache = st.cache.asset_yield.clone();
    let st = st.clone();
    cache
        .try_get_with(chain_id, async move {
            let rows = asset_yield::list(&st.pool, chain_id).await?;
            let out: Vec<YieldAssetOut> = rows.into_iter().map(to_out).collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

/// The treasury's earned-but-unswept fee, in underlying base units.
///
/// `accrued * gross / supply` in integers, which is the conversion the contract
/// performs. Deliberately *not* `accrued * index_ray / RAY`: `index_ray` is that
/// same ratio already rounded to 27 places, so rebuilding an amount from it adds
/// a rounding step the contract never takes and disagrees with it at the
/// boundary. `index_ray` is carried for display; this is the arithmetic.
///
/// The division goes through `BigInt` rather than `BigDecimal` for the same
/// reason: `BigDecimal`'s division applies a precision context and rounds, while
/// the contract truncates.
///
/// `None` before the first poll, and when the supply is zero — nothing has been
/// minted, so there is no share to convert. That is not a fee of zero, and
/// dividing by it would panic.
fn fee_underlying(row: &YieldRow) -> Option<BigDecimal> {
    let accrued = row.accrued_fee_normalized.as_ref()?;
    let total = row.total_normalized.as_ref()?;
    let gross = row.gross.as_ref()?;

    let supply = total + accrued;
    if supply.is_zero() {
        return None;
    }
    let fee = accrued.to_bigint()? * gross.to_bigint()? / supply.to_bigint()?;
    Some(BigDecimal::from(fee))
}

/// Two units on one row, and each field converts by the one it is in.
///
/// `gross`, `idle` and the fee are underlying and go through `decimals`.
/// `total_normalized`, `accrued_fee_normalized` and `index_ray` are not
/// denominated in the token at all — normalized share units and a RAY-scaled
/// ratio — so they are reported raw. Scaling those by `decimals` would produce a
/// number that looks like an amount and is not one.
fn to_out(row: YieldRow) -> YieldAssetOut {
    let decimals = row.decimals;
    let accrued_fee = fee_underlying(&row);
    // One conversion for every underlying field, so an amount cannot end up on
    // the wire having skipped `decimals`.
    let tokens = |base: Option<&BigDecimal>| base.and_then(|b| whole_tokens_str(b, decimals));
    YieldAssetOut {
        chain_id: row.chain_id,
        asset_id_u64: row.asset_id_u64,
        token_hex: row.token_hex,
        symbol: row.symbol,
        venue_hex: row.venue_hex,
        buffer_bps: row.buffer_bps,
        perf_bps: row.perf_bps,
        halted: row.halted,
        gross: tokens(row.gross.as_ref()),
        idle: tokens(row.idle.as_ref()),
        accrued_fee: tokens(accrued_fee.as_ref()),
        total_normalized: row.total_normalized.as_ref().map(plain_amount),
        accrued_fee_normalized: row.accrued_fee_normalized.as_ref().map(plain_amount),
        index_ray: row.index_ray.as_ref().map(plain_amount),
        block_number: row.block_number,
        updated_at: row.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// One RAY: the scale `index_ray` is expressed in.
    const RAY: &str = "1000000000000000000000000000";

    fn bd(s: &str) -> BigDecimal {
        BigDecimal::from_str(s).unwrap()
    }

    /// An asset bound to a venue that the poller has not reached yet: the
    /// event-sourced half only.
    fn bound(decimals: Option<i16>) -> YieldRow {
        YieldRow {
            chain_id: 1,
            asset_id_u64: 7,
            token_hex: "aa".to_string(),
            decimals,
            symbol: Some("AA".to_string()),
            venue_hex: "bb".to_string(),
            buffer_bps: 500,
            perf_bps: 1_000,
            halted: false,
            total_normalized: None,
            accrued_fee_normalized: None,
            idle: None,
            gross: None,
            index_ray: None,
            block_number: None,
            updated_at: None,
        }
    }

    /// The same asset once a poll has filled the state half. `index_ray` is
    /// passed separately so a test can set it to the rounded ratio the contract
    /// would publish.
    fn polled(
        decimals: Option<i16>,
        total: &str,
        accrued: &str,
        gross: &str,
        index_ray: &str,
    ) -> YieldRow {
        YieldRow {
            total_normalized: Some(bd(total)),
            accrued_fee_normalized: Some(bd(accrued)),
            idle: Some(bd("0")),
            gross: Some(bd(gross)),
            index_ray: Some(bd(index_ray)),
            block_number: Some(99),
            updated_at: Some(1_700_000_000),
            ..bound(decimals)
        }
    }

    /// The whole reason the conversion takes `gross` and `supply` rather than
    /// the published index.
    ///
    /// Supply 3 backed by 10 underlying: the true rate is 10/3, which `index_ray`
    /// can only carry to 27 places. Converting the treasury's whole 3 units
    /// through that rounded index gives 9; the contract's own `n * gross /
    /// supply` gives 10, and 10 is what the treasury can actually sweep.
    #[test]
    fn the_fee_converts_by_gross_over_supply_not_by_the_rounded_index() {
        let rounded_index = "3333333333333333333333333333"; // floor(10 * RAY / 3)
        let out = to_out(polled(Some(0), "0", "3", "10", rounded_index));
        assert_eq!(out.accrued_fee.as_deref(), Some("10"));
        // What rebuilding from the index would have produced.
        assert_ne!(out.accrued_fee.as_deref(), Some("9"));
    }

    /// Truncation, not rounding: the contract divides in integers, so a fee that
    /// does not divide evenly loses the remainder rather than gaining a unit.
    #[test]
    fn the_fee_truncates_the_way_integer_division_does() {
        // 1 * 10 / 3 = 3.33..., and the treasury can sweep 3.
        let out = to_out(polled(Some(0), "2", "1", "10", RAY));
        assert_eq!(out.accrued_fee.as_deref(), Some("3"));
    }

    /// A binding is created by an event and the state arrives on a later poll, so
    /// the gap is a normal state and must render as an asset with unknown
    /// numbers, not as a missing asset.
    #[test]
    fn a_bound_asset_reports_its_venue_before_the_first_poll() {
        let out = to_out(bound(Some(18)));
        assert_eq!(out.venue_hex, "bb");
        assert_eq!(out.buffer_bps, 500);
        assert_eq!(out.perf_bps, 1_000);
        assert!(!out.halted);
        assert_eq!(out.gross, None);
        assert_eq!(out.idle, None);
        assert_eq!(out.accrued_fee, None);
        assert_eq!(out.index_ray, None);
        assert_eq!(out.updated_at, None);
    }

    /// Nothing minted means no share to convert. Reporting `0` would claim the
    /// treasury is owed nothing, which is a different statement from having no
    /// rate to price it at.
    #[test]
    fn a_zero_supply_reports_no_fee_rather_than_zero() {
        let out = to_out(polled(Some(18), "0", "0", "0", RAY));
        assert_eq!(out.accrued_fee, None);
        // The supply itself is still reported: it is known, and it is zero.
        assert_eq!(out.total_normalized.as_deref(), Some("0"));
    }

    /// Same rule as every other amount on the API: without decimals a base-unit
    /// figure would be wrong by orders of magnitude, so none is reported.
    #[test]
    fn unresolved_decimals_report_no_token_amounts() {
        let out = to_out(polled(None, "1", "1", "1000", RAY));
        assert_eq!(out.gross, None);
        assert_eq!(out.idle, None);
        assert_eq!(out.accrued_fee, None);
    }

    /// Normalized units are not denominated in the token, so `decimals` must not
    /// touch them — otherwise an 18-decimal asset would report a supply of
    /// `0.000000000000000001` where the pool holds one unit.
    #[test]
    fn normalized_units_and_the_index_are_reported_raw() {
        let out = to_out(polled(Some(18), "1", "2", "3000000000000000000", RAY));
        assert_eq!(out.total_normalized.as_deref(), Some("1"));
        assert_eq!(out.accrued_fee_normalized.as_deref(), Some("2"));
        assert_eq!(out.index_ray.as_deref(), Some(RAY));
        // While the underlying figure beside them does scale.
        assert_eq!(out.gross.as_deref(), Some("3"));
    }

    /// A halt stops accrual; it does not unbind the venue, and the contract has
    /// no event that would. The asset stays in the listing so the halt is
    /// visible rather than the asset vanishing from it.
    #[test]
    fn a_halted_asset_is_still_listed() {
        let row = YieldRow {
            halted: true,
            ..polled(Some(18), "1", "0", "1000000000000000000", RAY)
        };
        let out = to_out(row);
        assert!(out.halted);
        assert_eq!(out.gross.as_deref(), Some("1"));
    }
}
