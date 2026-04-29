use crate::app::AppState;
use crate::domain::dto::{PathQuery, TreeStateQuery};
use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::{MerkleProofOut, TreeStateOut};
use crate::services;
use axum::Json;
use axum::extract::{Path, Query, State};

#[utoipa::path(
    get,
    path = "/v1/path/{cm}",
    tag = "tree",
    params(
        ("cm" = String, Path, description = "0x-prefixed 32-byte commitment hex"),
        PathQuery,
    ),
    responses((status = 200, body = MerkleProofOut))
)]
#[tracing::instrument(skip(st), fields(chain_id = q.chain_id, cm = %cm_hex))]
pub async fn get_path(
    State(st): State<AppState>,
    Path(cm_hex): Path<String>,
    Query(q): Query<PathQuery>,
) -> AppResult<Json<MerkleProofOut>> {
    let stripped = cm_hex.strip_prefix("0x").unwrap_or(&cm_hex);
    let cm = hex::decode(stripped).map_err(|e| AppError::BadRequest(format!("cm hex: {}", e)))?;
    if cm.len() != 32 {
        return Err(AppError::BadRequest(format!(
            "cm length {} != 32",
            cm.len()
        )));
    }
    Ok(Json(services::tree::path(&st, q.chain_id, &cm).await?))
}

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
