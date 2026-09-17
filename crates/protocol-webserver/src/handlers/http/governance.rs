use crate::app::AppState;
use crate::domain::error::AppResult;
use crate::domain::responses::{ProposalDetailOut, ProposalsPageOut, VotesPageOut};
use crate::services;
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct PageQuery {
    /// Required: proposal ids are per governor, so there is no all-chains form.
    pub chain_id: i64,
    /// Opaque `nextCursor` from the previous page.
    pub cursor: Option<String>,
    /// Page size, 1..=100; default 20. Out-of-range values are clamped.
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ChainQuery {
    pub chain_id: i64,
}

/// Proposals on one chain's governor, newest first, with indexed tallies.
///
/// No proposal state: a client reads `state()` from the governor, since it
/// depends on the clock and on quorum at the snapshot.
#[utoipa::path(
    get,
    path = "/v1/governance/proposals",
    tag = "governance",
    params(PageQuery),
    responses(
        (status = 200, body = ProposalsPageOut),
        (status = 400, description = "malformed cursor"),
        (status = 404, description = "chain not served by this deployment"),
    )
)]
pub async fn list_proposals(
    State(st): State<AppState>,
    Query(q): Query<PageQuery>,
) -> AppResult<Json<ProposalsPageOut>> {
    Ok(Json(
        services::governance::list_proposals(&st, q.chain_id, q.cursor.as_deref(), q.limit).await?,
    ))
}

/// One proposal, with its description and the calls it makes.
#[utoipa::path(
    get,
    path = "/v1/governance/proposals/{proposalId}",
    tag = "governance",
    params(
        ("proposalId" = String, Path, description = "Decimal uint256"),
        ChainQuery,
    ),
    responses(
        (status = 200, body = ProposalDetailOut),
        (status = 400, description = "proposalId is not a decimal uint256"),
        (status = 404, description = "unknown chain or proposal"),
    )
)]
pub async fn get_proposal(
    State(st): State<AppState>,
    Path(proposal_id): Path<String>,
    Query(q): Query<ChainQuery>,
) -> AppResult<Json<ProposalDetailOut>> {
    Ok(Json(
        services::governance::get_proposal(&st, q.chain_id, &proposal_id).await?,
    ))
}

/// Votes cast on one proposal, newest first.
#[utoipa::path(
    get,
    path = "/v1/governance/proposals/{proposalId}/votes",
    tag = "governance",
    params(
        ("proposalId" = String, Path, description = "Decimal uint256"),
        PageQuery,
    ),
    responses(
        (status = 200, body = VotesPageOut),
        (status = 400, description = "malformed proposalId or cursor"),
        (status = 404, description = "unknown chain or proposal"),
    )
)]
pub async fn list_votes(
    State(st): State<AppState>,
    Path(proposal_id): Path<String>,
    Query(q): Query<PageQuery>,
) -> AppResult<Json<VotesPageOut>> {
    Ok(Json(
        services::governance::list_votes(
            &st,
            q.chain_id,
            &proposal_id,
            q.cursor.as_deref(),
            q.limit,
        )
        .await?,
    ))
}
