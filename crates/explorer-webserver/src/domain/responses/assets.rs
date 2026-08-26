use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetOut {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub token_hex: String,
    /// Circuit capacity parameter (`baseUnits / scale` must fit `uint48`), not a
    /// decimals normalizer. Use `decimals` to render an amount.
    pub scale: String,
    /// ERC20 decimals. `null` until the indexer has read it from the chain;
    /// treat as unknown and render no amount rather than assuming 18.
    pub decimals: Option<i16>,
    /// ERC20 symbol. `null` until the indexer has read it, or when the token
    /// does not implement `symbol()`. Render the asset id rather than a guess.
    pub symbol: Option<String>,
    /// Spot USD price of one whole token. `null` when the provider does not know
    /// the token, which covers local test tokens and uncovered chains, or when
    /// the provider was unreachable. Absence means unknown, never zero.
    pub price_usd: Option<f64>,
    /// Provider's timestamp for `price_usd`, so a client can age the quote.
    /// `null` whenever `price_usd` is.
    pub price_at: Option<i64>,
}
