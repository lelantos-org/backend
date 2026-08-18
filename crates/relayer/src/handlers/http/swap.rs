use crate::app::AppState;
use crate::domain::dto::SubmitSwapPayload;
use crate::domain::error::AppResult;
use crate::domain::responses::RelayerSubmitResponse;
use crate::handlers::http::submission;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = payload.chain_id, adapter = %payload.swap.adapter))]
pub async fn submit_swap(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<SubmitSwapPayload>,
) -> AppResult<Json<RelayerSubmitResponse>> {
    let pipeline = st.swap_pipeline(payload.chain_id)?;
    submission::submit(st, headers, payload, |p| async move {
        pipeline.process(p).await
    })
    .await
}
