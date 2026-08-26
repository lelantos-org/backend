use crate::app::AppState;
use crate::domain::dto::EstimateDepositRequest;
use crate::domain::error::AppResult;
use crate::domain::responses::EstimateResponse;
use axum::Json;
use axum::extract::State;
use tracing::instrument;

/// What a deposit must pay this relayer for the flush that will commit it.
///
/// Quoted against `flushBatch` rather than a spend: the wallet escrows the
/// deposit itself, and the recovered cost is this deposit's share of a batch the
/// relayer later proves and broadcasts.
#[instrument(skip_all, fields(chain_id = req.chain_id))]
pub async fn estimate_deposit(
    State(st): State<AppState>,
    Json(req): Json<EstimateDepositRequest>,
) -> AppResult<Json<EstimateResponse>> {
    let pipeline = st.flush_pipeline(req.chain_id)?;
    Ok(Json(pipeline.estimate().await?))
}
