//! Spot USD prices for the registered catalog.
//!
//! The token set comes from the same `assets` table `/v1/assets` reads, so a
//! token is priced iff it is catalogued: the two routes cannot disagree about
//! which tokens exist.

use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::{PriceOut, PricesResponse};
use crate::services;
use prices::{TokenKey, TokenPrice};
use std::collections::HashMap;
use std::sync::Arc;

/// Prices for every chain this deployment serves.
///
/// Never fails on the provider's account. A dead or slow upstream yields fewer
/// rows — in the limit, none — because a wallet that cannot show a dollar figure
/// must still show a balance.
///
/// Cached under the unit key, which is distinct from the cache inside
/// `PriceService`: that one spares the provider, this one spares the database.
/// Every open wallet tab polls this, and the read behind it walks the catalog
/// for every configured chain.
///
/// The whole body is cached rather than its rows, so serving it is a refcount
/// bump: the route takes no parameters, so every caller in a TTL gets the same
/// object.
pub async fn list(st: &AppState) -> AppResult<Arc<PricesResponse>> {
    let cache = st.cache.prices.clone();
    let st = st.clone();
    services::cached(&cache, (), async move {
        // One query for every configured chain: a per-chain call would take
        // a pooled connection each.
        let rows = asset_registry::list_for_chains(&st.pool, &st.chain_ids()).await?;
        // Hex once per asset: it is both the price-lookup key and, with a
        // `0x`, the wire field.
        let keys = unique_token_keys(
            rows.into_iter()
                .map(|(chain_id, row)| TokenKey::new(chain_id, hex::encode(&row.token))),
        );

        // One upstream call for every chain at once, and only for tokens the
        // cache has not already answered for.
        let priced = st.prices.for_tokens(&keys).await;

        Ok(Arc::new(PricesResponse {
            prices: to_rows(&keys, &priced),
        }))
    })
    .await
}

/// The distinct `(chain, token)` pairs behind a set of registered assets.
///
/// The catalog is keyed by asset id and this route is keyed by token address,
/// and that relation is many-to-one: a yield asset is registered alongside the
/// plain asset it shadows and shares its ERC-20, differing only in the venue
/// binding. Mapping the catalog straight to keys therefore names the same pair
/// once per id — five times over on Ethereum — and a caller reads a price by
/// address, so those repeats are one fact restated rather than two facts.
///
/// Dropped here rather than in [`to_rows`] so the body carries one row per
/// token: the price lookup dedups its own requests, but a repeated key would
/// otherwise be repeated on the wire.
fn unique_token_keys(keys: impl Iterator<Item = TokenKey>) -> Vec<TokenKey> {
    let mut out: Vec<TokenKey> = keys.collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Pair each asked-about token with its price, dropping the ones that have none.
///
/// The drop is the contract: a token the provider does not know is absent from
/// the body, never carried with `price_usd: 0.0`.
fn to_rows(keys: &[TokenKey], priced: &HashMap<TokenKey, TokenPrice>) -> Vec<PriceOut> {
    let mut out: Vec<PriceOut> = keys
        .iter()
        .filter_map(|key| {
            let price = priced.get(key)?;
            Some(PriceOut {
                chain_id: key.chain.get(),
                token: format!("0x{}", key.address),
                price_usd: price.price_usd,
                price_at: price.quoted_at,
            })
        })
        .collect();
    // Sorted so one deployment's body is byte-identical between requests, which
    // is what lets a cache — ours or the edge's — treat it as one object.
    out.sort_by(|a, b| (a.chain_id, &a.token).cmp(&(b.chain_id, &b.token)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(chain_id: i64, hex: &str) -> TokenKey {
        TokenKey::new(chain_id, hex)
    }

    fn price(price_usd: f64) -> TokenPrice {
        TokenPrice {
            price_usd,
            decimals: Some(18),
            quoted_at: 7,
        }
    }

    /// The production shape: on Ethereum five yield ids shadow a plain id and
    /// share its ERC-20, so the catalog hands over the same pair repeatedly.
    #[test]
    fn a_yield_asset_sharing_its_plain_asset_erc20_yields_one_key() {
        let weth = key(1, "c02a");
        let usdc = key(1, "a0b8");

        let keys =
            unique_token_keys([weth.clone(), usdc.clone(), weth.clone(), usdc.clone()].into_iter());

        assert_eq!(keys, vec![usdc, weth], "one key per token, sorted");
    }

    /// A token on two chains is two facts, not a duplicate: the pair carries the
    /// chain, and the same address prices independently on each.
    #[test]
    fn the_same_token_on_two_chains_keeps_both_keys() {
        let mainnet = key(1, "a0b8");
        let base = key(8453, "a0b8");

        let keys = unique_token_keys([mainnet.clone(), base.clone(), mainnet.clone()].into_iter());

        assert_eq!(keys, vec![mainnet, base]);
    }

    #[test]
    fn an_unpriced_token_is_omitted_rather_than_zeroed() {
        let known = key(1, "a0b8");
        let unknown = key(1, "dead");
        let priced = HashMap::from([(known.clone(), price(3.5))]);

        let rows = to_rows(&[known, unknown], &priced);

        assert_eq!(rows.len(), 1, "the unpriced token must not appear at all");
        assert_eq!(rows[0].token, "0xa0b8");
        assert_eq!(rows[0].price_usd, 3.5);
        assert_eq!(rows[0].price_at, 7);
    }

    #[test]
    fn nothing_priced_yields_an_empty_body_not_an_error() {
        // What a wallet sees on the local anvil stack, and whenever the provider
        // is unreachable: a normal, empty answer.
        let rows = to_rows(&[key(31337, "a0b8")], &HashMap::new());
        assert!(rows.is_empty());
    }

    #[test]
    fn rows_are_ordered_regardless_of_chain_iteration_order() {
        let a = key(8453, "bbbb");
        let b = key(1, "cccc");
        let c = key(1, "aaaa");
        let priced = HashMap::from([
            (a.clone(), price(1.0)),
            (b.clone(), price(2.0)),
            (c.clone(), price(3.0)),
        ]);

        let rows = to_rows(&[a, b, c], &priced);

        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.chain_id, r.token.as_str()))
            .collect();
        assert_eq!(got, [(1, "0xaaaa"), (1, "0xcccc"), (8453, "0xbbbb")]);
    }

    /// `AssetOut::token` is `format!("0x{}", hex::encode(..))`. A client joins
    /// the catalog and the price table by that string, so this must match it
    /// character for character.
    #[test]
    fn the_token_field_is_spelled_like_the_catalog() {
        let k = key(1, "a0b8");
        let rows = to_rows(
            std::slice::from_ref(&k),
            &HashMap::from([(k.clone(), price(1.0))]),
        );
        assert_eq!(rows[0].token, "0xa0b8");
    }
}
