use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::YieldIndexResponse;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct YieldIndexQuery {
    /// Required. The body is one chain's whole history, so there is no
    /// all-chains form: it would be large and no client needs more than the
    /// chain it is connected to.
    pub chain_id: i64,
}

/// The recorded yield-index history for one chain.
///
/// Lets a client recover what a note was worth when it was credited without
/// asking the chain about that note's block — which would tell whoever answered
/// which blocks the wallet holds notes in. Every caller on a chain gets the same
/// body.
///
/// An asset with no recorded reading is absent. A note whose block predates the
/// history has no basis and must be reported as unknown rather than assumed.
#[utoipa::path(
    get,
    path = "/v1/yield-index",
    tag = "registry",
    params(YieldIndexQuery),
    responses(
        (status = 200, body = YieldIndexResponse),
        (status = 404, description = "chain not served by this deployment"),
    )
)]
pub async fn yield_index(
    State(st): State<AppState>,
    Query(q): Query<YieldIndexQuery>,
) -> AppResult<Json<Arc<YieldIndexResponse>>> {
    Ok(Json(services::yield_index::get(&st, q.chain_id).await?))
}
