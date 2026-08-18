use crate::app::AppState;
use crate::domain::dto::SubmitSpendPayload;
use crate::domain::error::AppResult;
use crate::domain::responses::EstimateResponse;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = payload.chain_id, kind = ?payload.kind))]
pub async fn estimate_spend(
    State(st): State<AppState>,
    Json(payload): Json<SubmitSpendPayload>,
) -> AppResult<Json<EstimateResponse>> {
    let pipeline = st.spend_pipeline(payload.chain_id)?;
    Ok(Json(pipeline.estimate(payload).await?))
}
