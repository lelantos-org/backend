use crate::app::AppState;
use crate::domain::dto::EstimateSpendRequest;
use crate::domain::error::AppResult;
use crate::domain::responses::EstimateResponse;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = req.chain_id, kind = ?req.kind))]
pub async fn estimate_spend(
    State(st): State<AppState>,
    Json(req): Json<EstimateSpendRequest>,
) -> AppResult<Json<EstimateResponse>> {
    let pipeline = st.spend_pipeline(req.chain_id)?;
    Ok(Json(pipeline.estimate(req.kind).await?))
}
