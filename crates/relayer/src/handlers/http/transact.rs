use crate::adapters::parse::parse_b32;
use crate::app::AppState;
use crate::domain::dto::SubmitSpendPayload;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::RelayerSubmitResponse;
use crate::services::nullifier_guard;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = payload.chain_id, kind = ?payload.kind))]
pub async fn submit_spend(
    State(st): State<AppState>,
    Json(payload): Json<SubmitSpendPayload>,
) -> AppResult<Json<RelayerSubmitResponse>> {
    let chain_id = payload.chain_id;
    let pipeline = st
        .spend_pipelines
        .get(&chain_id)
        .ok_or(AppError::UnknownChain(chain_id))?
        .clone();
    let nfs = [
        parse_b32(&payload.pub_inputs.nullifier[0])?.0,
        parse_b32(&payload.pub_inputs.nullifier[1])?.0,
    ];
    let _nf_guard =
        nullifier_guard::reserve_and_check(&st.pending_nullifiers, &st.pool, chain_id, nfs).await?;
    let receipt = pipeline.process(payload).await?;
    Ok(Json(RelayerSubmitResponse {
        tx_hash: format!("0x{}", hex::encode(receipt.tx_hash)),
    }))
}
