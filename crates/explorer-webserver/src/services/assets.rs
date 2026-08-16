use crate::adapters::TokenKey;
use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::AssetOut;
use crate::repositories::assets;
use std::sync::Arc;

pub async fn list(st: &AppState, chain_id: Option<i64>) -> AppResult<Arc<Vec<AssetOut>>> {
    let cache = st.cache.assets.clone();
    let st = st.clone();
    cache
        .try_get_with(chain_id, async move {
            let rows = assets::list(&st.pool, chain_id).await?;
            // Hex once per asset: it is both the price-lookup key and the
            // wire field.
            let keyed: Vec<(TokenKey, _)> = rows
                .into_iter()
                .map(|a| ((a.chain_id, hex::encode(&a.token)), a))
                .collect();
            // One upstream call for the whole registry, and only for tokens
            // the price cache has not already answered for.
            let prices =
                super::prices::for_tokens(&st, keyed.iter().map(|(k, _)| k.clone()).collect())
                    .await;

            let out: Vec<AssetOut> = keyed
                .into_iter()
                .map(|(key, a)| {
                    let price = prices.get(&key);
                    AssetOut {
                        chain_id: a.chain_id,
                        asset_id_u64: a.asset_id_u64,
                        token_hex: key.1,
                        scale: a.scale.to_string(),
                        decimals: a.decimals,
                        symbol: a.symbol,
                        price_usd: price.map(|p| p.price_usd),
                        price_at: price.map(|p| p.quoted_at),
                    }
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}
