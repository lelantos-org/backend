use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::models::{Quote, QuoteRequest};
use axum::Json;
use axum::extract::State;
use tracing::{debug, info, warn};

/// Hard cap on user-supplied slippage; higher values almost certainly
/// mean percent was passed where basis points were expected.
const MAX_SLIPPAGE_BPS: u16 = 5_000;

#[utoipa::path(
    post,
    path = "/v1/quotes",
    tag = "quotes",
    request_body = QuoteRequest,
    responses(
        (status = 200, description = "Best route across racing venues", body = Quote),
        (status = 400, description = "Bad request (invalid slippage / same-token swap)"),
        (status = 404, description = "Chain not configured"),
        (status = 422, description = "No liquidity for the pair"),
        (status = 502, description = "All venues failed or RPC error"),
    )
)]
/// A quote names the pair and the amount a user is about to trade, minutes or
/// seconds before the swap reaches the chain. Nothing on chain ties that quote
/// to the requester — but this log line and the access-log line share a
/// timestamp, and `POST /relayer/v1/swap` follows from the same client shortly
/// after. Logging the pair here is therefore the only server-side record of
/// that correlation, so none of `token_in`, `token_out`, `amount_in` or
/// `expected_out` is recorded. `expected_out` alone is enough: with the pair
/// and the venue it reconstructs `amount_in`.
///
/// What is left — chain, venue, outcome class — is what operating the service
/// actually needs. Diagnosing one pair is a reproduction, not a log grep.
pub async fn post_quote(
    State(st): State<AppState>,
    Json(req): Json<QuoteRequest>,
) -> AppResult<Json<Quote>> {
    validate(&req)?;
    let chain_id = req.chain_id;
    debug!(chain_id, "quote requested");
    match st.quote_service.best_quote(req).await {
        Ok(q) => {
            info!(chain_id, venue = ?q.venue, "quote served");
            Ok(Json(q))
        }
        Err(e) => {
            warn!(chain_id, error = e.class(), "quote failed");
            Err(e)
        }
    }
}

fn validate(req: &QuoteRequest) -> AppResult<()> {
    if req.slippage_bps > MAX_SLIPPAGE_BPS {
        return Err(AppError::BadRequest(format!(
            "slippage_bps must be <= {MAX_SLIPPAGE_BPS}"
        )));
    }
    if req.token_in == req.token_out {
        return Err(AppError::BadRequest("token_in == token_out".into()));
    }
    Ok(())
}
