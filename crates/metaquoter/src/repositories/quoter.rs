use crate::domain::error::AppError;
use crate::domain::models::{Quote, QuoteRequest, Venue};
use async_trait::async_trait;

/// One implementation per venue. The service layer races every quoter whose
/// `supports_chain` returns true and picks the highest `expected_out`.
#[async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait Quoter: Send + Sync {
    fn venue(&self) -> Venue;

    fn supports_chain(&self, chain_id: u64) -> bool;

    async fn quote(&self, req: &QuoteRequest) -> Result<Quote, AppError>;
}
