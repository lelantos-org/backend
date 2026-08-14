use crate::app::AppState;
use crate::domain::address::normalize;
use crate::domain::dto::screen::{MAX_BATCH, ScreenBatchRequest, ScreenRequest};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::ScreenOut;
use axum::Json;
use axum::extract::State;
use std::sync::Arc;

#[utoipa::path(
    post,
    path = "/v1/screen",
    tag = "screen",
    request_body = ScreenRequest,
    responses(
        (status = 200, body = ScreenOut),
        (status = 400, description = "unknown chain or malformed address"),
    )
)]
pub async fn screen(
    State(st): State<AppState>,
    Json(req): Json<ScreenRequest>,
) -> AppResult<Json<Arc<ScreenOut>>> {
    let addr = normalize(&req.chain, &req.address)?;
    let mut out = st.screening.screen(vec![addr]).await?;
    // `screen` returns one verdict per input, in order.
    Ok(Json(out.remove(0)))
}

#[utoipa::path(
    post,
    path = "/v1/screen/batch",
    tag = "screen",
    request_body = ScreenBatchRequest,
    responses(
        (status = 200, body = [ScreenOut], description = "verdicts in request order"),
        (status = 400, description = "empty batch, batch too large, or malformed address"),
    )
)]
pub async fn screen_batch(
    State(st): State<AppState>,
    Json(req): Json<ScreenBatchRequest>,
) -> AppResult<Json<Vec<Arc<ScreenOut>>>> {
    if req.addresses.is_empty() {
        return Err(AppError::BadRequest("addresses must not be empty".into()));
    }
    if req.addresses.len() > MAX_BATCH {
        return Err(AppError::BadRequest(format!(
            "at most {MAX_BATCH} addresses per batch"
        )));
    }
    let addrs = req
        .addresses
        .iter()
        .map(|a| normalize(&req.chain, a))
        .collect::<AppResult<Vec<_>>>()?;
    Ok(Json(st.screening.screen(addrs).await?))
}
