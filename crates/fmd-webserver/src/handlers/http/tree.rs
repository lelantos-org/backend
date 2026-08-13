//! Tree state only. There is deliberately no per-commitment path endpoint:
//! asking for the proof of one `cm` tells the server (and every cache and
//! proxy log on the way) exactly which note the caller is about to spend.
//! Clients build the tree from the commitment chunk feed and derive paths
//! locally instead.

use crate::app::AppState;
use crate::domain::dto::TreeStateQuery;
use crate::domain::error::AppResult;
use crate::domain::responses::TreeStateOut;
use crate::services;
use axum::Json;
use axum::extract::{Query, State};

#[utoipa::path(
    get,
    path = "/v1/tree-state",
    tag = "tree",
    params(TreeStateQuery),
    responses((status = 200, body = TreeStateOut))
)]
#[tracing::instrument(skip(st), fields(chain_id = q.chain_id))]
pub async fn get_tree_state(
    State(st): State<AppState>,
    Query(q): Query<TreeStateQuery>,
) -> AppResult<Json<TreeStateOut>> {
    Ok(Json(services::tree::tree_state(&st, q.chain_id).await?))
}
