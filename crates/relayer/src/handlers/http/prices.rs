use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{PriceOut, PricesResponse};
use crate::repositories::assets;
use axum::Json;
use axum::extract::State;
use prices::{TokenKey, TokenPrice, for_tokens};
use std::collections::HashMap;
use std::sync::Arc;

/// Spot USD prices for every registered asset the provider can price.
///
/// Split from `/chains` rather than folded into it: a wallet reads the registry
/// once and holds it, so a price delivered there would be fixed at page load.
/// Here it has its own cadence and its own cache header.
///
/// Never fails on the provider's account. A dead or slow upstream yields fewer
/// rows — in the limit, none — because a wallet that cannot show a dollar figure
/// must still show a balance.
pub async fn prices(State(st): State<AppState>) -> AppResult<Json<PricesResponse>> {
    let cached = st.prices_response.clone();
    let rows = cached
        .try_get_with((), async move {
            // One query for every configured chain: a per-chain call would take a
            // pooled connection each, and the relayer pool holds four.
            let chain_ids: Vec<i64> = st.spend_pipelines.keys().copied().collect();
            // Hex once per asset: it is both the price-lookup key and, with a
            // `0x`, the wire field.
            let keys = unique_token_keys(
                assets::list_for_chains(&st.pool, &chain_ids)
                    .await?
                    .into_iter()
                    .map(|(chain_id, row)| (chain_id, hex::encode(&row.token))),
            );

            // One upstream call for every chain at once, and only for tokens the
            // cache has not already answered for.
            let priced = for_tokens(&st.prices, &st.price_cache, &keys).await;

            Ok::<_, AppError>(Arc::new(to_rows(&keys, &priced)))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))?;

    Ok(Json(PricesResponse { prices: rows }))
}

/// The distinct `(chain, token)` pairs behind a set of registered assets.
///
/// The registry is keyed by asset id and this endpoint is keyed by token
/// address, and that relation is many-to-one: a yield asset is registered
/// alongside the plain asset it shadows and shares its ERC-20, differing only in
/// the venue binding. Mapping the registry straight to keys therefore names the
/// same pair once per id — five times over on Ethereum — and a caller reads a
/// price by address, so those repeats are one fact restated rather than two
/// facts.
///
/// Dropped here rather than in `to_rows` because `for_tokens` dedups nothing
/// either: left in, the duplicates also ask the provider about the same token
/// twice in the same request.
fn unique_token_keys(keys: impl Iterator<Item = TokenKey>) -> Vec<TokenKey> {
    let mut out: Vec<TokenKey> = keys.collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Pair each asked-about token with its price, dropping the ones that have none.
///
/// The drop is the contract: a token the provider does not know is absent from
/// the body, never carried with `price_usd: 0.0`. Zero is a price a token could
/// really have, so emitting it for "unknown" would put a figure on screen that
/// reads as a measurement.
fn to_rows(keys: &[TokenKey], priced: &HashMap<TokenKey, TokenPrice>) -> Vec<PriceOut> {
    let mut out: Vec<PriceOut> = keys
        .iter()
        .filter_map(|key| {
            let price = priced.get(key)?;
            Some(PriceOut {
                chain_id: key.0,
                token: format!("0x{}", key.1),
                price_usd: price.price_usd,
                price_at: price.quoted_at,
            })
        })
        .collect();
    // `spend_pipelines` is a HashMap, so the chain order the keys were built in
    // is not stable across processes. Sorting keeps one deployment's body
    // byte-identical between requests, which is what lets a cache — ours or the
    // edge's — treat it as one object.
    out.sort_by(|a, b| (a.chain_id, &a.token).cmp(&(b.chain_id, &b.token)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(chain_id: i64, hex: &str) -> TokenKey {
        (chain_id, hex.to_string())
    }

    fn price(price_usd: f64) -> TokenPrice {
        TokenPrice {
            price_usd,
            decimals: Some(18),
            quoted_at: 7,
        }
    }

    /// The production shape: on Ethereum five yield ids shadow a plain id and
    /// share its ERC-20, so the registry hands over the same pair twice.
    #[test]
    fn a_yield_asset_sharing_its_plain_asset_erc20_yields_one_key() {
        let weth = key(1, "c02a");
        let usdc = key(1, "a0b8");

        // plain WETH, plain USDC, then the yield ids shadowing each.
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

    /// End-to-end over the real provider: keys in, `PriceOut` rows out.
    ///
    /// The local anvil stack cannot exercise this: `llama_chain` maps 31337 to
    /// nothing, so `/v1/prices` is empty there and a deployment is the first place
    /// the priced path runs. Ignored by default since it needs the network.
    ///
    /// Run with `cargo test -p relayer -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "hits the live DefiLlama API"]
    async fn live_prices_map_all_the_way_to_wire_rows() {
        use prices::PriceClient;
        use std::time::Duration;

        let usdc = key(1, "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        let unpriceable = key(31337, "0000000000000000000000000000000000000001");
        let keys = [usdc.clone(), unpriceable];

        let client =
            PriceClient::new("https://coins.llama.fi".into(), Duration::from_secs(15)).unwrap();
        let cache = prices::PriceCache::builder().max_capacity(8).build();

        let priced = for_tokens(&client, &cache, &keys).await;
        let rows = to_rows(&keys, &priced);

        assert_eq!(rows.len(), 1, "only the priced token belongs on the wire");
        assert_eq!(rows[0].chain_id, 1);
        assert_eq!(rows[0].token, format!("0x{}", usdc.1));
        assert!(rows[0].price_usd > 0.5 && rows[0].price_usd < 2.0);
        assert!(rows[0].price_at > 1_700_000_000);
    }

    #[test]
    fn the_token_field_is_spelled_like_the_registry() {
        // `TokenOut::token` is `format!("0x{}", hex::encode(..))`. A client joins
        // the two by string, so this must match it character for character.
        let k = key(1, "a0b8");
        let rows = to_rows(
            std::slice::from_ref(&k),
            &HashMap::from([(k.clone(), price(1.0))]),
        );
        assert_eq!(rows[0].token, "0xa0b8");
    }
}
