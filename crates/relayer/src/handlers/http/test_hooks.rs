//! `/test/bundler/{chain_id}/*`: deterministic bundling for end-to-end tests.
//!
//! Holding a chain's batcher lets a test queue several operations and release
//! them as one bundle, rather than racing the batcher's natural batching. Mounted
//! only when `[test_hooks] enabled = true`.

use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::services::pipeline::batcher::QueuedItem;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

/// Stop dispatching; operations queue until release.
pub async fn hold(State(st): State<AppState>, Path(chain_id): Path<i64>) -> AppResult<StatusCode> {
    st.batcher(chain_id)?.hold();
    Ok(StatusCode::NO_CONTENT)
}

/// Operations queued behind a hold, oldest first.
pub async fn queue(
    State(st): State<AppState>,
    Path(chain_id): Path<i64>,
) -> AppResult<Json<Vec<QueuedItem>>> {
    Ok(Json(st.batcher(chain_id)?.queued()))
}

/// Dispatch the queue as normal and resume.
pub async fn release(
    State(st): State<AppState>,
    Path(chain_id): Path<i64>,
) -> AppResult<StatusCode> {
    st.batcher(chain_id)?.release();
    Ok(StatusCode::NO_CONTENT)
}
