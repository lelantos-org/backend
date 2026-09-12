//! `POST /v1/quotes`.

use crate::app::AppState;
use crate::domain::error::{AppError, AppResult};
use crate::domain::models::{Quote, QuoteRequest};
use alloy::primitives::Address;
use axum::Json;
use axum::extract::State;
use tracing::{debug, info, warn};

/// Hard cap on caller-supplied slippage. Higher values indicate percent was
/// passed where basis points were expected.
const MAX_SLIPPAGE_BPS: u16 = 5_000;

#[utoipa::path(
    post,
    path = "/v1/quotes",
    tag = "quotes",
    request_body = QuoteRequest,
    responses(
        (status = 200, description = "Best route across racing venues", body = Quote),
        (status = 400, description = "Bad request (invalid slippage / same-token swap / zero amount)"),
        (status = 404, description = "Chain not configured"),
        (status = 422, description = "No liquidity for the pair"),
        (status = 502, description = "Every venue failed or was unreachable"),
        (status = 504, description = "Every venue exceeded the race deadline"),
    )
)]
/// A quote names the pair and the amount a caller is about to trade, seconds or
/// minutes before the swap reaches the chain. Nothing on chain ties the quote to
/// the requester, but this log line shares a timestamp with the access-log line
/// and `POST /relayer/v1/swap` follows from the same client shortly after, so
/// logging the pair here would be the only server-side record of that
/// correlation. None of `token_in`, `token_out`, `amount_in` or `expected_out`
/// is recorded; `expected_out` alone reconstructs `amount_in` given the pair and
/// the venue.
///
/// Chain, venue and outcome class are recorded, which is what operating the
/// service requires.
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

/// Requests no venue could answer usefully, rejected before any `eth_call`.
///
/// Each of these would otherwise reach a quoter and come back as a 422 or a
/// venue-specific 400, which tells the caller the pair has no pool when the
/// problem is the request.
fn validate(req: &QuoteRequest) -> AppResult<()> {
    if req.slippage_bps > MAX_SLIPPAGE_BPS {
        return Err(AppError::BadRequest(format!(
            "slippage_bps must be <= {MAX_SLIPPAGE_BPS}"
        )));
    }
    if req.token_in == req.token_out {
        return Err(AppError::BadRequest("token_in == token_out".into()));
    }
    // Native-ETH V4 pools are keyed on the zero address and are not quoted here,
    // and no V3 pool holds it. Left through, a zero side quotes against a pool
    // that cannot exist.
    if req.token_in == Address::ZERO || req.token_out == Address::ZERO {
        return Err(AppError::BadRequest(
            "token_in and token_out must be ERC20 addresses".into(),
        ));
    }
    // A zero input quotes zero out without reverting, so it wins its own race
    // and is served as a valid route with `expected_out` of 0.
    if req.amount_in.is_zero() {
        return Err(AppError::BadRequest("amount_in must be > 0".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{U256, address};

    const A: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    const B: Address = address!("6b175474e89094c44da98b954eedeac495271d0f");

    fn req() -> QuoteRequest {
        QuoteRequest {
            chain_id: 1,
            token_in: A,
            token_out: B,
            amount_in: U256::from(1_000_000u64),
            slippage_bps: 50,
        }
    }

    #[test]
    fn a_well_formed_request_passes() {
        validate(&req()).unwrap();
    }

    /// The typo guard: a caller passing a percentage where bps were expected
    /// would otherwise get a `min_out` floor far below what they meant.
    #[test]
    fn slippage_above_the_cap_is_rejected() {
        let mut r = req();
        r.slippage_bps = MAX_SLIPPAGE_BPS + 1;
        assert!(validate(&r).is_err());

        r.slippage_bps = MAX_SLIPPAGE_BPS;
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn a_same_token_swap_is_rejected() {
        let mut r = req();
        r.token_out = r.token_in;
        assert!(validate(&r).is_err());
    }

    /// Both sides, since only one of them needs to be zero for the pool key to
    /// name something no venue here quotes.
    #[test]
    fn a_zero_address_side_is_rejected() {
        let mut r = req();
        r.token_in = Address::ZERO;
        assert!(validate(&r).is_err());

        let mut r = req();
        r.token_out = Address::ZERO;
        assert!(validate(&r).is_err());
    }

    /// A zero amount does not revert at either lens: it quotes zero out, wins
    /// its own race, and is served as a 200 with `expected_out` of 0.
    #[test]
    fn a_zero_amount_is_rejected() {
        let mut r = req();
        r.amount_in = U256::ZERO;
        assert!(validate(&r).is_err());
    }
}
