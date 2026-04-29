use crate::app::AppState;
use crate::domain::dto::SubmitSwapPayload;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::EstimateResponse;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = payload.chain_id, adapter = %payload.swap.adapter))]
pub async fn estimate_swap(
    State(st): State<AppState>,
    Json(payload): Json<SubmitSwapPayload>,
) -> AppResult<Json<EstimateResponse>> {
    let chain_id = payload.chain_id;
    let pipeline = st
        .swap_pipelines
        .get(&chain_id)
        .ok_or(AppError::UnknownChain(chain_id))?
        .clone();
    let calldata = pipeline.dry_build_calldata(payload).await?;
    let to = pipeline.submitter.pool_address;
    let resp = pipeline.fee_quoter.quote_for_calldata(to, calldata).await?;
    Ok(Json(resp))
}
