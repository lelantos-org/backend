//! Spot USD prices, as `/v1/prices` publishes them.
//!
//! Moved here from the relayer, where it sat alongside routes that carry
//! submissions. A price is a property of the *token*, identical for every caller
//! and for every relayer serving the chain, so it belongs with the catalog it
//! prices: a self-hosted relayer has no business stating what WETH is worth, and
//! a wallet that read it from one would have a figure it cannot cross-check.
//!
//! A separate route from `/v1/assets` rather than a field on `AssetOut`: the
//! catalog moves when the indexer registers an asset, while a price is stale
//! within the minute, so folding them together would mean refetching the catalog
//! to move a price.

use serde::Serialize;
use utoipa::ToSchema;

/// One token's spot quote.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PriceOut {
    pub chain_id: i64,
    /// 0x-prefixed ERC-20 address, spelled exactly as `AssetOut::token` spells
    /// it. A client joins the two by string, so the two must not drift.
    pub token: String,
    pub price_usd: f64,
    /// The provider's own timestamp, not the time this body was built, so a
    /// client can age the quote rather than trusting that it is fresh.
    pub price_at: i64,
}

/// Every registered token the provider could price, across every chain served.
///
/// A token the provider does not know is **absent** rather than carried with
/// `priceUsd: 0.0`. Zero is a price a token could really have, so emitting it
/// for "unknown" would put a figure on screen that reads as a measurement.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PricesResponse {
    pub prices: Vec<PriceOut>,
}
