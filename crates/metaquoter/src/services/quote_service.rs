//! Racing a request across every venue that serves the requested chain.

use crate::domain::error::{AppError, AppResult};
use crate::domain::models::{Quote, QuoteRequest};
use crate::repositories::quoter::Quoter;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

#[async_trait]
pub trait QuoteService: Send + Sync {
    async fn best_quote(&self, req: QuoteRequest) -> AppResult<Quote>;
}

/// Races every quoter that supports the requested chain and returns the route
/// with the highest `expected_out`. Each quoter has its own deadline; slow
/// quoters are dropped from the race rather than failing the request.
pub struct RacingQuoteService {
    quoters: Vec<Arc<dyn Quoter>>,
    deadline: Duration,
}

impl RacingQuoteService {
    pub fn new(quoters: Vec<Arc<dyn Quoter>>, deadline: Duration) -> Self {
        Self { quoters, deadline }
    }
}

#[async_trait]
impl QuoteService for RacingQuoteService {
    async fn best_quote(&self, req: QuoteRequest) -> AppResult<Quote> {
        let active: Vec<_> = self
            .quoters
            .iter()
            .filter(|q| q.supports_chain(req.chain_id))
            .cloned()
            .collect();

        if active.is_empty() {
            return Err(AppError::UnsupportedChain(req.chain_id));
        }

        let futs = active.into_iter().map(|q| {
            let req = req.clone();
            let venue = q.venue();
            let deadline = self.deadline;
            async move {
                match tokio::time::timeout(deadline, async move { q.quote(&req).await }).await {
                    Ok(Ok(quote)) => Ok(quote),
                    Ok(Err(e)) => {
                        debug!(?venue, class = e.class(), "quoter produced no quote");
                        Err(e)
                    }
                    Err(_) => {
                        warn!(?venue, "quoter timed out");
                        Err(AppError::Timeout)
                    }
                }
            }
        });

        // A losing quoter's error is kept rather than discarded. It is unused
        // whenever any venue answered, but when none did it is the only thing
        // that distinguishes a pair with no pool from a node that was down, and
        // those are a 422 the caller should stop retrying and a 502 they should
        // retry. Collapsing both to `AllVenuesFailed` told every caller to
        // retry a pair that will never quote.
        let mut quotes = Vec::new();
        let mut failures = Vec::new();
        for outcome in futures::future::join_all(futs).await {
            match outcome {
                Ok(q) => quotes.push(q),
                Err(e) => failures.push(e),
            }
        }

        // Ties on output go to the cheaper route. Two venues quoting the same
        // number is rare, but when it happens gas is the whole difference in
        // what the caller nets.
        quotes
            .into_iter()
            .max_by(|a, b| {
                a.expected_out
                    .cmp(&b.expected_out)
                    .then(b.gas_estimate.cmp(&a.gas_estimate))
            })
            .ok_or_else(|| AppError::worst(failures))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::Venue;
    use crate::repositories::quoter::MockQuoter;
    use alloy::primitives::{Address, Bytes, U256};

    fn req(chain_id: u64) -> QuoteRequest {
        QuoteRequest {
            chain_id,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            amount_in: U256::from(1_000_000u64),
            slippage_bps: 50,
        }
    }

    fn quote(out: u64) -> Quote {
        quote_with_gas(out, 700_000)
    }

    fn quote_with_gas(out: u64, gas_estimate: u64) -> Quote {
        Quote {
            venue: Venue::UniV3,
            adapter: Address::ZERO,
            route: Bytes::new(),
            expected_out: U256::from(out),
            min_out: U256::from(out * 995 / 1000),
            gas_estimate,
            quoted_at: 0,
            masp_fee: U256::ZERO,
            masp_fee_bps: 0,
        }
    }

    fn svc(quoters: Vec<Arc<dyn Quoter>>) -> RacingQuoteService {
        RacingQuoteService::new(quoters, Duration::from_secs(1))
    }

    fn mock(
        venue: Venue,
        outcome: impl Fn() -> Result<Quote, AppError> + Send + 'static,
    ) -> Arc<dyn Quoter> {
        let mut q = MockQuoter::new();
        q.expect_supports_chain().return_const(true);
        q.expect_venue().return_const(venue);
        q.expect_quote()
            .returning(move |_| Box::pin(std::future::ready(outcome())));
        Arc::new(q)
    }

    fn mock_ok(out: u64) -> Arc<dyn Quoter> {
        mock(Venue::UniV3, move || Ok(quote(out)))
    }

    fn mock_ok_with_gas(out: u64, gas: u64) -> Arc<dyn Quoter> {
        mock(Venue::UniV3, move || Ok(quote_with_gas(out, gas)))
    }

    fn mock_err(make: impl Fn() -> AppError + Send + 'static) -> Arc<dyn Quoter> {
        mock(Venue::UniV3, move || Err(make()))
    }

    /// A quoter that outlives the race deadline, to exercise the timeout arm.
    fn mock_slow() -> Arc<dyn Quoter> {
        let mut q = MockQuoter::new();
        q.expect_supports_chain().return_const(true);
        q.expect_venue().return_const(Venue::UniV4);
        q.expect_quote().returning(|_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(quote(1))
            })
        });
        Arc::new(q)
    }

    #[tokio::test]
    async fn picks_max_expected_out() {
        let got = svc(vec![mock_ok(100), mock_ok(200)])
            .best_quote(req(1))
            .await
            .unwrap();
        assert_eq!(got.expected_out, U256::from(200u64));
    }

    /// Equal output is decided by gas, which is then the entire difference in
    /// what the caller nets.
    #[tokio::test]
    async fn ties_on_output_go_to_the_cheaper_route() {
        let got = svc(vec![
            mock_ok_with_gas(200, 900_000),
            mock_ok_with_gas(200, 700_000),
        ])
        .best_quote(req(1))
        .await
        .unwrap();
        assert_eq!(got.gas_estimate, 700_000);
    }

    #[tokio::test]
    async fn unsupported_chain_when_no_active_quoters() {
        let mut q = MockQuoter::new();
        q.expect_supports_chain().return_const(false);
        q.expect_venue().return_const(Venue::UniV3);
        let err = svc(vec![Arc::new(q)])
            .best_quote(req(999))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::UnsupportedChain(999)));
    }

    /// Every venue agreeing the pair has no pool is a 422 the caller should
    /// stop retrying, not the 502 this used to return for every failure alike.
    #[tokio::test]
    async fn unanimous_no_liquidity_surfaces_as_no_liquidity() {
        let err = svc(vec![
            mock_err(|| AppError::NoLiquidity),
            mock_err(|| AppError::NoLiquidity),
        ])
        .best_quote(req(1))
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::NoLiquidity), "{err}");
    }

    /// One venue's node being unreachable must not let the other venue's
    /// verdict be reported as the truth about the pair.
    #[tokio::test]
    async fn an_unreachable_venue_outranks_another_venues_verdict() {
        let err = svc(vec![
            mock_err(|| AppError::NoLiquidity),
            mock_err(|| AppError::Rpc("connection refused".into())),
        ])
        .best_quote(req(1))
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Rpc(_)), "{err}");
    }

    /// The caller's own input reaches them as a 400. Before the race kept
    /// errors, `amount_in` above `uint128` came back as a 502 the SDK retried.
    #[tokio::test]
    async fn a_caller_error_survives_the_race() {
        let err = svc(vec![
            mock_err(|| AppError::NoLiquidity),
            mock_err(|| AppError::BadRequest("amount_in exceeds uint128".into())),
        ])
        .best_quote(req(1))
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)), "{err}");
    }

    /// A venue that answers wins outright; a slower or broken one never
    /// contributes its error.
    #[tokio::test(start_paused = true)]
    async fn one_answer_beats_any_number_of_failures() {
        let got = svc(vec![
            mock_err(|| AppError::Rpc("connection refused".into())),
            mock_slow(),
            mock_ok(42),
        ])
        .best_quote(req(1))
        .await
        .unwrap();
        assert_eq!(got.expected_out, U256::from(42u64));
    }

    #[tokio::test(start_paused = true)]
    async fn every_quoter_timing_out_is_a_timeout() {
        let err = svc(vec![mock_slow()]).best_quote(req(1)).await.unwrap_err();
        assert!(matches!(err, AppError::Timeout), "{err}");
    }
}
