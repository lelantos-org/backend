use crate::adapters::{TokenKey, TokenPrice};
use crate::app::AppState;
use crate::domain::amount::whole_tokens_str;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{ChainLockedOut, LockedAssetOut};
use crate::repositories::asset_locked::{self, LockedRow};
use bigdecimal::ToPrimitive;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub async fn by_chain(st: &AppState, chain_id: Option<i64>) -> AppResult<Arc<Vec<ChainLockedOut>>> {
    let cache = st.cache.locked.clone();
    let st = st.clone();
    cache
        .try_get_with(chain_id, async move {
            let rows = asset_locked::totals(&st.pool, chain_id).await?;
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

/// One asset's balance, priced where a price exists.
///
/// Locked is `in - out` in base units, converted afterwards: subtracting whole
/// tokens would round each side first and lose a wei of difference between two
/// dust amounts. A negative result is reported as-is; escrow cannot owe money, so
/// a negative balance indicates missed deposits and clamping it to zero would
/// hide that.
fn locked_asset(row: LockedRow, prices: &HashMap<TokenKey, TokenPrice>) -> LockedAssetOut {
    let locked_base = &row.in_base - &row.out_base;
    let locked_usd = prices
        .get(&(row.chain_id, row.token_hex.clone()))
        .and_then(|p| super::prices::to_usd(locked_base.to_f64()?, row.decimals, p));
    LockedAssetOut {
        asset_id_u64: row.asset_id_u64,
        token_hex: row.token_hex,
        symbol: row.symbol,
        amount: whole_tokens_str(&locked_base, row.decimals),
        locked_usd,
        last_ts: row.last_ts,
    }
}

/// Largest dollar balance first, with unpriced entries trailing.
///
/// `Option`'s own ordering sorts `None` below every `Some`, which this reverses:
/// an unpriced asset has no place on a dollar scale, so it trails rather than
/// ranking as worthless. `unwrap_or(Equal)` fires only on a NaN price, which
/// `collect` refuses to store.
fn richest_first(a: Option<f64>, b: Option<f64>) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

/// Collapse per-asset balances into one entry per chain.
///
/// The dollar total is the only figure that adds up across assets, and it counts
/// what it excludes: an asset with no price lands in `unpriced_assets` rather
/// than contributing zero.
fn fold(rows: Vec<LockedRow>, prices: &HashMap<TokenKey, TokenPrice>) -> Vec<ChainLockedOut> {
    let mut chains: BTreeMap<i64, ChainLockedOut> = BTreeMap::new();
    for row in rows {
        let chain_id = row.chain_id;
        let asset = locked_asset(row, prices);
        let entry = chains.entry(chain_id).or_insert_with(|| ChainLockedOut {
            chain_id,
            locked_usd: None,
            unpriced_assets: 0,
            assets: Vec::new(),
        });
        match asset.locked_usd {
            Some(usd) => *entry.locked_usd.get_or_insert(0.0) += usd,
            None => entry.unpriced_assets += 1,
        }
        entry.assets.push(asset);
    }

    let mut out: Vec<ChainLockedOut> = chains.into_values().collect();
    for chain in &mut out {
        chain.assets.sort_by(|a, b| {
            richest_first(a.locked_usd, b.locked_usd).then(a.asset_id_u64.cmp(&b.asset_id_u64))
        });
    }
    // Ties break on the id so the order is stable between requests; every chain
    // ties while nothing can be priced.
    out.sort_by(|a, b| richest_first(a.locked_usd, b.locked_usd).then(a.chain_id.cmp(&b.chain_id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;
    use std::str::FromStr;

    /// A balance row. `asset_id_u64` derives from the token so each test names an
    /// asset once, by the identity it also prices and asserts on.
    fn row(chain_id: i64, token: &str, decimals: i16, r#in: &str, out: &str) -> LockedRow {
        LockedRow {
            chain_id,
            asset_id_u64: 1000 + i64::from(token.as_bytes()[0]),
            token_hex: token.to_string(),
            decimals: Some(decimals),
            symbol: Some(token.to_uppercase()),
            in_base: BigDecimal::from_str(r#in).unwrap(),
            out_base: BigDecimal::from_str(out).unwrap(),
            last_ts: 42,
        }
    }

    fn priced(chain_id: i64, token: &str, price_usd: f64) -> (TokenKey, TokenPrice) {
        (
            (chain_id, token.to_string()),
            TokenPrice {
                price_usd,
                // The asset's own decimals take precedence, so the feed need not
                // report any.
                decimals: None,
                quoted_at: 0,
            },
        )
    }

    fn amount_of<'a>(chain: &'a ChainLockedOut, token: &str) -> &'a LockedAssetOut {
        chain
            .assets
            .iter()
            .find(|a| a.token_hex == token)
            .unwrap_or_else(|| panic!("no {token} in chain {}", chain.chain_id))
    }

    fn tokens(chain: &ChainLockedOut) -> Vec<&str> {
        chain.assets.iter().map(|a| a.token_hex.as_str()).collect()
    }

    #[test]
    fn locked_is_deposits_minus_withdrawals_in_whole_tokens() {
        // 14 WETH in, 1.5 out.
        let rows = vec![row(
            1,
            "aa",
            18,
            "14000000000000000000",
            "1500000000000000000",
        )];
        let out = fold(rows, &HashMap::new());
        assert_eq!(amount_of(&out[0], "aa").amount.as_deref(), Some("12.5"));
    }

    #[test]
    fn the_difference_is_taken_before_the_conversion() {
        // Two amounts that each round to nothing in whole tokens still differ by
        // a wei, which must survive the conversion.
        let out = fold(vec![row(1, "aa", 18, "3", "1")], &HashMap::new());
        assert_eq!(
            amount_of(&out[0], "aa").amount.as_deref(),
            Some("0.000000000000000002")
        );
    }

    #[test]
    fn a_chain_totals_its_assets_in_dollars_only() {
        // 2 USDC left, plus 2 WETH at $3000.
        let rows = vec![
            row(1, "aa", 6, "3000000", "1000000"),
            row(1, "bb", 18, "2000000000000000000", "0"),
        ];
        let prices = HashMap::from([priced(1, "aa", 1.0), priced(1, "bb", 3000.0)]);
        let out = fold(rows, &prices);
        assert_eq!(out.len(), 1);
        assert!((out[0].locked_usd.unwrap() - 6002.0).abs() < 1e-6);
        assert_eq!(out[0].unpriced_assets, 0);
    }

    #[test]
    fn an_unpriced_asset_is_counted_and_left_out_of_the_dollar_total() {
        let rows = vec![
            row(1, "aa", 6, "5000000", "0"),
            row(1, "bb", 6, "9000000", "0"),
        ];
        let out = fold(rows, &HashMap::from([priced(1, "aa", 1.0)]));
        assert!((out[0].locked_usd.unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(out[0].unpriced_assets, 1);
        // The token amount is still reported: unknown price, known quantity.
        let unpriced = amount_of(&out[0], "bb");
        assert_eq!(unpriced.amount.as_deref(), Some("9"));
        assert_eq!(unpriced.locked_usd, None);
    }

    #[test]
    fn a_chain_with_nothing_priceable_totals_nothing_rather_than_zero() {
        let out = fold(vec![row(1, "aa", 6, "5000000", "0")], &HashMap::new());
        assert_eq!(out[0].locked_usd, None);
        assert_eq!(out[0].unpriced_assets, 1);
    }

    #[test]
    fn unresolved_decimals_report_no_amount_rather_than_base_units() {
        let unresolved = LockedRow {
            decimals: None,
            ..row(1, "aa", 6, "5000000", "0")
        };
        let out = fold(vec![unresolved], &HashMap::new());
        assert_eq!(amount_of(&out[0], "aa").amount, None);
    }

    #[test]
    fn a_negative_balance_is_reported_not_clamped() {
        // Escrow cannot owe money, so this indicates missed deposits; clamping to
        // zero would hide it.
        let out = fold(vec![row(1, "aa", 6, "1000000", "4000000")], &HashMap::new());
        assert_eq!(amount_of(&out[0], "aa").amount.as_deref(), Some("-3"));
    }

    #[test]
    fn chains_come_back_richest_first() {
        let rows = vec![
            row(1, "aa", 6, "1000000", "0"),
            row(10, "bb", 6, "9000000", "0"),
        ];
        let prices = HashMap::from([priced(1, "aa", 1.0), priced(10, "bb", 1.0)]);
        let out = fold(rows, &prices);
        assert_eq!(
            out.iter().map(|c| c.chain_id).collect::<Vec<_>>(),
            vec![10, 1]
        );
    }

    #[test]
    fn unpriced_chains_keep_a_stable_order_behind_the_priced_ones() {
        let rows = vec![
            row(10, "bb", 6, "9000000", "0"),
            row(1, "cc", 6, "9000000", "0"),
            row(5, "aa", 6, "1000000", "0"),
        ];
        let out = fold(rows, &HashMap::from([priced(5, "aa", 1.0)]));
        assert_eq!(
            out.iter().map(|c| c.chain_id).collect::<Vec<_>>(),
            vec![5, 1, 10]
        );
    }

    #[test]
    fn an_unpriced_asset_trails_the_priced_ones() {
        let rows = vec![
            row(1, "bb", 6, "9000000", "0"),
            row(1, "aa", 6, "1000000", "0"),
        ];
        let out = fold(rows, &HashMap::from([priced(1, "aa", 1.0)]));
        assert_eq!(tokens(&out[0]), vec!["aa", "bb"]);
    }
}
