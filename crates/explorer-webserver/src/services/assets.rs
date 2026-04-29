use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::AssetOut;
use crate::repositories::assets;
use std::sync::Arc;

pub async fn list(st: &AppState, chain_id: Option<i64>) -> AppResult<Arc<Vec<AssetOut>>> {
    let pool = st.pool.clone();
    st.cache
        .assets
        .try_get_with(chain_id, async move {
            let rows = assets::list(&pool, chain_id).await?;
            let out: Vec<AssetOut> = rows
                .into_iter()
                .map(|a| AssetOut {
                    chain_id: a.chain_id,
                    asset_id_u64: a.asset_id_u64,
                    token_hex: hex::encode(&a.token),
                    scale: a.scale.to_string(),
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}
