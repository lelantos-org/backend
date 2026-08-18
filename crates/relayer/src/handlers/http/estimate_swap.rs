use crate::app::AppState;
use crate::domain::dto::SubmitSwapPayload;
use crate::domain::error::AppResult;
use crate::domain::responses::EstimateResponse;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = payload.chain_id, adapter = %payload.swap.adapter))]
pub async fn estimate_swap(
    State(st): State<AppState>,
    Json(payload): Json<SubmitSwapPayload>,
) -> AppResult<Json<EstimateResponse>> {
    let pipeline = st.swap_pipeline(payload.chain_id)?;
    Ok(Json(pipeline.estimate(payload).await?))
}
