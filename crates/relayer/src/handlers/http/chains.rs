use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::{ChainHealth, ChainsResponse};
use crate::services::tree::field_to_hex;
use axum::Json;
use axum::extract::State;

pub async fn chains(State(st): State<AppState>) -> AppResult<Json<ChainsResponse>> {
    let mut chains = Vec::with_capacity(st.spend_pipelines.len());
    for (chain_id, pipeline) in st.spend_pipelines.iter() {
        let mirror = pipeline.mirror.lock().await;
        let root = mirror.current_root()?;
        chains.push(ChainHealth {
            chain_id: *chain_id,
            committed_count: mirror.committed_count() as i64,
            current_root_hex: field_to_hex(&root),
            masp_address: pipeline.submitter.pool_address.to_checksum(None),
            desynced: mirror.is_desynced(),
        });
    }
    chains.sort_by_key(|c| c.chain_id);
    Ok(Json(ChainsResponse { chains }))
}
