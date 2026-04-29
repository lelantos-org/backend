use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{ChainFlowOut, CountPoint, TreeAdvanceOut};
use crate::repositories::tree_advances;
use std::collections::BTreeMap;
use std::sync::Arc;

pub async fn list(
    st: &AppState,
    chain_id: Option<i64>,
    since_start_index: Option<i64>,
    limit: i64,
) -> AppResult<Arc<Vec<TreeAdvanceOut>>> {
    let key = (chain_id, since_start_index, limit);
    let pool = st.pool.clone();
    st.cache
        .tree_advances
        .try_get_with(key, async move {
            let rows = tree_advances::list(&pool, chain_id, since_start_index, limit).await?;
            let out: Vec<TreeAdvanceOut> = rows
                .into_iter()
                .map(|t| TreeAdvanceOut {
                    chain_id: t.chain_id,
                    block_number: t.block_number,
                    log_index: t.log_index,
                    start_index: t.start_index,
                    inserted: t.inserted,
                    old_root_hex: hex::encode(&t.old_root),
                    new_root_hex: hex::encode(&t.new_root),
                    tx_hash_hex: hex::encode(&t.tx_hash),
                    block_ts: t.block_ts,
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

pub async fn tx_counts(
    st: &AppState,
    chain_id: Option<i64>,
    bucket_sec: i64,
    since_ts: Option<i64>,
) -> AppResult<Arc<Vec<CountPoint>>> {
    let key = (chain_id, bucket_sec, since_ts);
    let pool = st.pool.clone();
    st.cache
        .tx_counts
        .try_get_with(key, async move {
            let rows = tree_advances::count_buckets(&pool, chain_id, bucket_sec, since_ts).await?;
            let out: Vec<CountPoint> = rows
                .into_iter()
                .map(|r| CountPoint {
                    ts: r.ts,
                    count: r.count,
                })
                .collect();
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}

pub async fn chain_flows_24h(st: &AppState, now_ts: i64) -> AppResult<Arc<Vec<ChainFlowOut>>> {
    let since = now_ts - 86_400;
    let hour_start = (since / 3600) * 3600;
    let pool = st.pool.clone();
    st.cache
        .chain_flows_24h
        .try_get_with(hour_start, async move {
            let rows = tree_advances::chain_flows_24h(&pool, hour_start).await?;

            let mut map: BTreeMap<i64, ChainFlowOut> = BTreeMap::new();
            for r in rows {
                let entry = map.entry(r.chain_id).or_insert_with(|| ChainFlowOut {
                    chain_id: r.chain_id,
                    inflow: 0,
                    outflow: 0,
                    hourly_in: vec![0; 24],
                    hourly_out: vec![0; 24],
                    tx_count: 0,
                });
                let slot = r.slot.clamp(0, 23) as usize;
                entry.hourly_in[slot] += r.count;
                entry.tx_count += r.count;
            }
            let mut out: Vec<ChainFlowOut> = map.into_values().collect();
            out.sort_by_key(|b| std::cmp::Reverse(b.tx_count));
            Ok::<_, AppError>(Arc::new(out))
        })
        .await
        .map_err(|e: Arc<AppError>| AppError::Internal(e.to_string()))
}
