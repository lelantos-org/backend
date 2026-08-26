use crate::adapters::{TokenKey, TokenPrice};
use crate::app::AppState;
use crate::domain::amount::{plain_amount, whole_tokens};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::FlowPoint;
use crate::repositories::asset_flows::{self, FlowBucketRow};
use bigdecimal::{BigDecimal, ToPrimitive};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

pub async fn flows(
    st: &AppState,
    chain_id: Option<i64>,
    asset_id_u64: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Arc<Vec<FlowPoint>>> {
    let key = (chain_id, asset_id_u64, bucket_sec, since_ts);
    let cache = st.cache.asset_flows.clone();
    let st = st.clone();
    cache
        .try_get_with(key, async move {
            let rows =
                asset_flows::flow_buckets(&st.pool, chain_id, asset_id_u64, bucket_sec, since_ts)
                    .await?;
            let keys: Vec<TokenKey> = rows
                .iter()
                .map(|r| (r.chain_id, r.token_hex.clone()))
                .collect();
            let prices = super::prices::for_tokens(&st, &keys).await;
            Ok::<_, AppError>(Arc::new(fold(rows, &prices)))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

#[derive(Default)]
struct Bucket {
    /// Whole tokens. Meaningful only while a single asset is in scope.
    tokens_in: BigDecimal,
    tokens_out: BigDecimal,
    in_usd: Option<f64>,
    out_usd: Option<f64>,
    unpriced_assets: i64,
}

/// Collapse per-asset rows into one point per bucket.
///
/// Dollars use the token's own decimals and the provider's current spot price
/// for every bucket in the range, so a 90-day window values three-month-old
/// volume at today's price. Clients should label the figure accordingly rather
/// than presenting it as value at the time.
///
/// Token amounts are emitted only when the response covers exactly one asset,
/// since two assets cannot be added in any token unit. With more than one in
/// scope the token fields are `null` and USD is the only total. An asset that
/// cannot be priced counts toward `unpriced_assets`, so a partial dollar sum is
/// distinguishable from a complete one.
fn fold(rows: Vec<FlowBucketRow>, prices: &HashMap<TokenKey, TokenPrice>) -> Vec<FlowPoint> {
    let single_asset = rows
        .iter()
        .map(|r| (r.chain_id, r.token_hex.as_str()))
        .collect::<HashSet<_>>()
        .len()
        <= 1;
    let mut tokens_known = single_asset;

    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();
    for r in rows {
        let b = buckets.entry(r.ts).or_default();

        match (
            whole_tokens(&r.in_base, r.decimals),
            whole_tokens(&r.out_base, r.decimals),
        ) {
            (Some(i), Some(o)) => {
                b.tokens_in += i;
                b.tokens_out += o;
            }
            // Decimals unresolved: report no amount rather than a wrong one.
            _ => tokens_known = false,
        }

        let usd = prices
            .get(&(r.chain_id, r.token_hex.clone()))
            .and_then(|p| {
                let into = super::prices::to_usd(r.in_base.to_f64()?, r.decimals, p)?;
                let out = super::prices::to_usd(r.out_base.to_f64()?, r.decimals, p)?;
                Some((into, out))
            });
        match usd {
            Some((into, out)) => {
                *b.in_usd.get_or_insert(0.0) += into;
                *b.out_usd.get_or_insert(0.0) += out;
            }
            None => b.unpriced_assets += 1,
        }
    }

    let emit_tokens = single_asset && tokens_known;
    buckets
        .into_iter()
        .map(|(ts, b)| FlowPoint {
            ts,
            // Already whole tokens; `plain_amount` only normalises the notation,
            // so a dust bucket cannot render as "2E-18".
            in_amount: emit_tokens.then(|| plain_amount(&b.tokens_in)),
            out_amount: emit_tokens.then(|| plain_amount(&b.tokens_out)),
            in_usd: b.in_usd,
            out_usd: b.out_usd,
            unpriced_assets: b.unpriced_assets,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn row(ts: i64, token: &str, decimals: Option<i16>, in_base: &str) -> FlowBucketRow {
        FlowBucketRow {
            ts,
            chain_id: 1,
            token_hex: token.to_string(),
            decimals,
            in_base: BigDecimal::from_str(in_base).unwrap(),
            out_base: BigDecimal::default(),
        }
    }

    fn priced(token: &str, price_usd: f64, decimals: u32) -> (TokenKey, TokenPrice) {
        (
            (1, token.to_string()),
            TokenPrice {
                price_usd,
                decimals: Some(decimals),
                quoted_at: 0,
            },
        )
    }

    #[test]
    fn a_single_asset_reports_whole_tokens() {
        // 14e18 base units of an 18-decimal token is 14 WETH, not 1.4e9 circuit
        // units and not 14e18 base units.
        let out = fold(
            vec![row(100, "aa", Some(18), "14000000000000000000")],
            &HashMap::new(),
        );
        assert_eq!(out[0].in_amount.as_deref(), Some("14"));
    }

    #[test]
    fn fractional_amounts_survive() {
        let out = fold(vec![row(100, "aa", Some(8), "150000000")], &HashMap::new());
        assert_eq!(out[0].in_amount.as_deref(), Some("1.5"));
    }

    #[test]
    fn two_assets_have_no_token_total() {
        // 14 WETH + 13 mWBTC is not 27 of anything.
        let out = fold(
            vec![
                row(100, "aa", Some(18), "14000000000000000000"),
                row(100, "bb", Some(8), "1300000000"),
            ],
            &HashMap::new(),
        );
        assert_eq!(out[0].in_amount, None);
        assert_eq!(out[0].out_amount, None);
    }

    #[test]
    fn unresolved_decimals_yield_no_token_amount() {
        let out = fold(
            vec![row(100, "aa", None, "14000000000000000000")],
            &HashMap::new(),
        );
        assert_eq!(out[0].in_amount, None);
    }

    #[test]
    fn usd_converts_each_asset_at_its_own_price() {
        let prices = HashMap::from([priced("aa", 1.0, 6), priced("bb", 3000.0, 18)]);
        let out = fold(
            vec![
                row(100, "aa", Some(6), "1000000"),
                row(100, "bb", Some(18), "2000000000000000000"),
            ],
            &prices,
        );
        let usd = out[0].in_usd.unwrap();
        assert!((usd - 6001.0).abs() < 1e-6, "got {usd}");
        assert_eq!(out[0].unpriced_assets, 0);
    }

    #[test]
    fn dollars_use_the_tokens_own_decimals_not_the_providers() {
        // The feed reports 18 decimals for a 6-decimal token; trusting it would
        // turn 1 USDC of flow into $1e-12.
        let prices = HashMap::from([priced("aa", 1.0, 18)]);
        let out = fold(vec![row(100, "aa", Some(6), "1000000")], &prices);
        assert!(
            (out[0].in_usd.unwrap() - 1.0).abs() < 1e-9,
            "{:?}",
            out[0].in_usd
        );
        assert_eq!(out[0].unpriced_assets, 0);
    }

    #[test]
    fn a_priced_asset_still_counts_in_dollars_before_decimals_are_backfilled() {
        // No token amount is possible without stored decimals, but the price
        // feed's are enough for the dollar total; withholding it would flag the
        // asset unpriced and could empty the whole range.
        let prices = HashMap::from([priced("aa", 2.0, 6)]);
        let out = fold(vec![row(100, "aa", None, "1000000")], &prices);
        assert_eq!(out[0].in_amount, None);
        assert!((out[0].in_usd.unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(out[0].unpriced_assets, 0);
    }

    #[test]
    fn an_unpriced_asset_is_flagged_and_excluded_from_dollars() {
        let prices = HashMap::from([priced("aa", 1.0, 6)]);
        let out = fold(
            vec![
                row(100, "aa", Some(6), "1000000"),
                row(100, "bb", Some(6), "9000000"),
            ],
            &prices,
        );
        assert!((out[0].in_usd.unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(out[0].unpriced_assets, 1);
    }

    #[test]
    fn a_bucket_with_no_prices_reports_null_usd() {
        let out = fold(vec![row(100, "aa", Some(6), "5000000")], &HashMap::new());
        assert_eq!(out[0].in_usd, None);
        assert_eq!(out[0].unpriced_assets, 1);
    }

    #[test]
    fn buckets_come_back_in_ascending_ts_order() {
        let out = fold(
            vec![row(300, "aa", Some(6), "1"), row(100, "aa", Some(6), "1")],
            &HashMap::new(),
        );
        assert_eq!(out.iter().map(|p| p.ts).collect::<Vec<_>>(), vec![100, 300]);
    }
}
