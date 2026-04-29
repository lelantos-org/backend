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

/// Races every quoter that supports the requested chain, returns the
/// route with the highest `expected_out`. Each quoter has an individual
/// deadline; slow quoters are dropped from the race rather than failing
/// the whole request.
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
                    Ok(Ok(quote)) => Some(quote),
                    Ok(Err(e)) => {
                        debug!(?venue, error = %e, "quoter returned error");
                        None
                    }
                    Err(_) => {
                        warn!(?venue, "quoter timed out");
                        None
                    }
                }
            }
        });

        let quotes: Vec<Quote> = futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect();

        quotes
            .into_iter()
            .max_by_key(|q| q.expected_out)
            .ok_or(AppError::AllVenuesFailed)
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
        Quote {
            venue: Venue::UniV3,
            adapter: Address::ZERO,
            route: Bytes::new(),
            expected_out: U256::from(out),
            min_out: U256::from(out * 995 / 1000),
            gas_estimate: 700_000,
            quoted_at: 0,
            masp_fee: U256::ZERO,
            masp_fee_bps: 0,
        }
    }

    fn svc(quoters: Vec<Arc<dyn Quoter>>) -> RacingQuoteService {
        RacingQuoteService::new(quoters, Duration::from_secs(1))
    }

    fn mock_ok(out: u64) -> Arc<dyn Quoter> {
        let mut q = MockQuoter::new();
        q.expect_supports_chain().return_const(true);
        q.expect_venue().return_const(Venue::UniV3);
        q.expect_quote()
            .returning(move |_| Box::pin(async move { Ok(quote(out)) }));
        Arc::new(q)
    }

    fn mock_err(err: AppError) -> Arc<dyn Quoter> {
        let mut q = MockQuoter::new();
        q.expect_supports_chain().return_const(true);
        q.expect_venue().return_const(Venue::UniV3);
        let err = std::sync::Mutex::new(Some(err));
        q.expect_quote().returning(move |_| {
            let e = err.lock().unwrap().take().unwrap_or(AppError::NoLiquidity);
            Box::pin(async move { Err(e) })
        });
        Arc::new(q)
    }

    fn mock_unsupported() -> Arc<dyn Quoter> {
        let mut q = MockQuoter::new();
        q.expect_supports_chain().return_const(false);
        q.expect_venue().return_const(Venue::UniV3);
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

    #[tokio::test]
    async fn unsupported_chain_when_no_active_quoters() {
        let err = svc(vec![mock_unsupported()])
            .best_quote(req(999))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::UnsupportedChain(999)));
    }

    #[tokio::test]
    async fn all_failing_returns_all_venues_failed() {
        let err = svc(vec![mock_err(AppError::NoLiquidity)])
            .best_quote(req(1))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::AllVenuesFailed));
    }
}
