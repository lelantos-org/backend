use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::FlowPoint;
use crate::repositories::asset_flows;
use std::sync::Arc;

pub async fn flows(
    st: &AppState,
    chain_id: Option<i64>,
    asset_id_u64: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Arc<Vec<FlowPoint>>> {
    let key = (chain_id, asset_id_u64, bucket_sec, since_ts);
    let pool = st.pool.clone();
    st.cache
        .asset_flows
        .try_get_with(key, async move {
            let rows =
                asset_flows::flow_buckets(&pool, chain_id, asset_id_u64, bucket_sec, since_ts)
                    .await?;
            let out: Vec<FlowPoint> = rows
                .into_iter()
                .map(|r| FlowPoint {
                    ts: r.ts,
                    in_amount: r.in_amount.to_string(),
                    out_amount: r.out_amount.to_string(),
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}
