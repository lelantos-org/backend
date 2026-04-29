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
pub async fn post_quote(
    State(st): State<AppState>,
    Json(req): Json<QuoteRequest>,
) -> AppResult<Json<Quote>> {
    validate(&req)?;
    debug!(
        chain_id = req.chain_id,
        token_in = %req.token_in,
        token_out = %req.token_out,
        amount_in = %req.amount_in,
        "quote requested"
    );
    let chain_id = req.chain_id;
    let token_in = req.token_in;
    let token_out = req.token_out;
    match st.quote_service.best_quote(req).await {
        Ok(q) => {
            info!(
                chain_id,
                token_in = %token_in,
                token_out = %token_out,
                expected_out = %q.expected_out,
                venue = ?q.venue,
                "quote served"
            );
            Ok(Json(q))
        }
        Err(e) => {
            warn!(chain_id, token_in = %token_in, token_out = %token_out, error = %e, "quote failed");
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
