use crate::app::AppState;
use crate::domain::dto::SubmitSpendPayload;
use crate::domain::error::AppResult;
use crate::domain::responses::RelayerSubmitResponse;
use crate::handlers::http::submission;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use tracing::instrument;

#[instrument(skip_all, fields(chain_id = payload.chain_id, kind = ?payload.kind))]
pub async fn submit_spend(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<SubmitSpendPayload>,
) -> AppResult<Json<RelayerSubmitResponse>> {
    let pipeline = st.spend_pipeline(payload.chain_id)?;
    submission::submit(st, headers, payload, |p| async move {
        pipeline.process(p).await
    })
    .await
}
