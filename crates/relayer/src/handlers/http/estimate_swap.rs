use crate::app::AppState;
use crate::domain::dto::EstimateSwapRequest;
use crate::domain::error::AppResult;
use crate::domain::responses::EstimateResponse;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = req.chain_id))]
pub async fn estimate_swap(
    State(st): State<AppState>,
    Json(req): Json<EstimateSwapRequest>,
) -> AppResult<Json<EstimateResponse>> {
    let pipeline = st.swap_pipeline(req.chain_id)?;
    Ok(Json(pipeline.estimate().await?))
}
