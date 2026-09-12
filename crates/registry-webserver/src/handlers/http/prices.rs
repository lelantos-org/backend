use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::PricesResponse;
use crate::services;
use axum::Json;
use axum::extract::State;
use std::sync::Arc;

/// Spot USD prices for every catalogued token the provider can price.
///
/// Split from `/v1/assets` rather than folded into it: a wallet reads the
/// catalog on a slow cadence and holds it, so a price delivered there would be
/// fixed at page load. Here it has its own cadence and its own cache header.
#[utoipa::path(
    get,
    path = "/v1/prices",
    tag = "registry",
    responses((status = 200, body = PricesResponse))
)]
pub async fn prices(State(st): State<AppState>) -> AppResult<Json<Arc<PricesResponse>>> {
    Ok(Json(services::prices::list(&st).await?))
}
