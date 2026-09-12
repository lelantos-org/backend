use crate::adapters::TokenKey;
use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::AssetOut;
use std::sync::Arc;

pub async fn list(st: &AppState, chain_id: Option<i64>) -> AppResult<Arc<Vec<AssetOut>>> {
    let cache = st.cache.assets.clone();
    let st = st.clone();
    super::cached(&cache, chain_id, async move {
        // The catalog rows come from `asset-registry` so this service and
        // the registry-webserver read one row shape; `chain_id` arrives
        // beside each row rather than inside it.
        let rows = match chain_id {
            Some(c) => asset_registry::list_for_chains(&st.pool, &[c]).await?,
            None => asset_registry::list_all(&st.pool).await?,
        };
        // Hex-encode once per asset: it serves as both the price-lookup key
        // and the wire field.
        let keyed: Vec<(TokenKey, _)> = rows
            .into_iter()
            .map(|(chain_id, a)| {
                (
                    TokenKey::new(chain_id, hex::encode(&a.token)),
                    (chain_id, a),
                )
            })
            .collect();
        // One upstream call for the whole registry, covering only tokens the
        // price cache has not already answered for.
        let prices = super::prices::for_tokens(
            &st,
            &keyed.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
        )
        .await;

        let out: Vec<AssetOut> = keyed
            .into_iter()
            .map(|(key, (chain_id, a))| {
                let price = prices.get(&key);
                AssetOut {
                    chain_id,
                    asset_id_u64: a.asset_id_u64,
                    token_hex: key.address,
                    scale: a.scale.to_string(),
                    decimals: a.decimals,
                    symbol: a.symbol,
                    price_usd: price.map(|p| p.price_usd),
                    price_at: price.map(|p| p.quoted_at),
                    deposit_bps: a.deposit_bps,
                    withdraw_bps: a.withdraw_bps,
                }
            })
            .collect();
        Ok(Arc::new(out))
    })
    .await
}
