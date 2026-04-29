use crate::domain::error::AppError;
use crate::domain::models::{Quote, QuoteRequest, Venue};
use async_trait::async_trait;

/// One impl per venue (UniV3, Curve, 1inch). Service layer races every
/// quoter that returns true from `supports_chain` and picks the highest
/// `expected_out`.
#[async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait Quoter: Send + Sync {
    fn venue(&self) -> Venue;

    fn supports_chain(&self, chain_id: u64) -> bool;

    async fn quote(&self, req: &QuoteRequest) -> Result<Quote, AppError>;
}
