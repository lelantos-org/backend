//! The price-source interface.

use crate::token::{TokenKey, TokenPrice};
use anyhow::Result;
use async_trait::async_trait;
use shared::chain::ChainId;
use std::collections::HashMap;

/// One implementation per upstream price source.
///
/// [`crate::PriceService`] consults providers in the order it was given them and
/// hands each one only the tokens whose chain it claims, so an implementation
/// never has to answer for a chain it does not cover.
#[async_trait]
pub trait PriceProvider: Send + Sync {
    /// Identifies the provider in logs. A failure names the source that failed,
    /// which is the only way to read a warning once there is more than one.
    fn name(&self) -> &'static str;

    /// Whether this provider can price tokens on `chain`.
    ///
    /// Separate from [`Self::fetch`] so a chain nobody covers — local anvil —
    /// costs no request at all rather than one per provider.
    fn supports_chain(&self, chain: ChainId) -> bool;

    /// Price whichever of `tokens` this provider knows.
    ///
    /// A token absent from the returned map is one the provider does not price,
    /// which is a successful answer. `Err` is reserved for a failed exchange
    /// with upstream. The service caches the first and not the second, so the
    /// distinction decides whether a token is asked about again on the next
    /// request or once per TTL.
    async fn fetch(&self, tokens: &[TokenKey]) -> Result<HashMap<TokenKey, TokenPrice>>;
}
