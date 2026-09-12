use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::PoolNotesOut;
use crate::repositories::pool_notes;
use std::sync::Arc;

pub async fn per_chain(st: &AppState, chain_id: Option<i64>) -> AppResult<Arc<Vec<PoolNotesOut>>> {
    let pool = st.pool.clone();
    super::cached(&st.cache.pool_notes, chain_id, async move {
        let rows = pool_notes::per_chain(&pool, chain_id).await?;
        let out: Vec<PoolNotesOut> = rows
            .into_iter()
            .map(|r| PoolNotesOut {
                chain_id: r.chain_id,
                leaves: r.leaves,
                fee_notes: r.fee_notes,
                last_ts: r.last_ts,
            })
            .collect();
        Ok(Arc::new(out))
    })
    .await
}
