use serde::Serialize;
use utoipa::ToSchema;

/// What one asset still holds in escrow on one chain, all-time deposits minus
/// all-time withdrawals.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LockedAssetOut {
    pub asset_id_u64: i64,
    pub token_hex: String,
    /// ERC20 symbol, `null` until the indexer has read it. Clients fall back to
    /// the address rather than inventing a label.
    pub symbol: Option<String>,
    /// Locked balance in **whole tokens**, as a decimal string. `null` when the
    /// token's decimals are still unresolved: the amount is unknown, and a base
    /// unit figure would be wrong by orders of magnitude.
    pub amount: Option<String>,
    /// The same balance in USD at the current spot price. `null` when the token
    /// has no usable price.
    pub locked_usd: Option<f64>,
    /// Newest flow behind this balance, so a client can age it.
    pub last_ts: i64,
}

/// One chain's escrowed balance.
///
/// **There is no cross-asset token total**, here as everywhere: `locked_usd` is
/// the only figure that adds up across assets, and it covers only the assets
/// that could be priced — `unpriced_assets` counts the rest, so a partial total
/// never passes for a complete one. Prices are current spot, applied to a
/// balance built from all of history, so this is what the pool holds valued
/// today.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainLockedOut {
    pub chain_id: i64,
    /// `null` when nothing on the chain could be priced.
    pub locked_usd: Option<f64>,
    pub unpriced_assets: i64,
    /// Per asset, largest dollar balance first; unpriced assets trail, ordered
    /// by registry id so the list is stable between requests.
    pub assets: Vec<LockedAssetOut>,
}
