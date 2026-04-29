use crate::app::AppState;
use crate::domain::dto::SubmitSpendPayload;
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::EstimateResponse;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = payload.chain_id, kind = ?payload.kind))]
pub async fn estimate_spend(
    State(st): State<AppState>,
    Json(payload): Json<SubmitSpendPayload>,
) -> AppResult<Json<EstimateResponse>> {
    let chain_id = payload.chain_id;
    let pipeline = st
        .spend_pipelines
        .get(&chain_id)
        .ok_or(AppError::UnknownChain(chain_id))?
        .clone();
    let calldata = pipeline.dry_build_calldata(payload).await?;
    let to = pipeline.submitter.pool_address;
    let resp = pipeline.fee_quoter.quote_for_calldata(to, calldata).await?;
    Ok(Json(resp))
}
