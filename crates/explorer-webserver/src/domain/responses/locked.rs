use serde::Serialize;
use utoipa::ToSchema;

/// How a locked balance was arrived at.
///
/// Two assets in the same response can be measured differently, so the figure
/// carries its own definition rather than leaving a caller to assume one.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum LockedBasis {
    /// All-time deposits minus all-time withdrawals. Exact for an asset held as
    /// plain custody, where nothing changes the balance but a flow.
    FlowDifference,
    /// What the pool and its venue actually hold, read from chain. Used for a
    /// yield asset, whose balance grows without any flow to observe.
    VenueHoldings,
}

/// What one asset still holds in escrow on one chain.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LockedAssetOut {
    pub asset_id_u64: i64,
    pub token_hex: String,
    /// ERC20 symbol, `null` until the indexer has read it. Clients fall back to
    /// the address rather than a synthesised label.
    pub symbol: Option<String>,
    /// Locked balance in whole tokens, as a decimal string. `null` while the
    /// token's decimals are unresolved, since a base-unit figure would be wrong
    /// by orders of magnitude.
    pub amount: Option<String>,
    /// The same balance in USD at the current spot price. `null` when the token
    /// has no usable price.
    pub locked_usd: Option<f64>,
    /// Newest flow behind this balance, so a client can age it.
    ///
    /// For a `venueHoldings` figure this ages the last *flow*, not the last
    /// index refresh: the balance itself is as fresh as the indexer's poll.
    pub last_ts: i64,
    /// Which definition produced `amount`.
    pub basis: LockedBasis,
}

/// One chain's escrowed balance.
///
/// There is no cross-asset token total. `locked_usd` is the only figure that
/// adds up across assets, and it covers only the assets that could be priced;
/// `unpriced_assets` counts the rest so a partial total is distinguishable from
/// a complete one. Prices are current spot applied to an all-time balance, so
/// this is what the pool holds valued today.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainLockedOut {
    pub chain_id: i64,
    /// `null` when nothing on the chain could be priced.
    pub locked_usd: Option<f64>,
    pub unpriced_assets: i64,
    /// Per asset, largest dollar balance first. Unpriced assets trail, ordered by
    /// registry id so the list is stable between requests.
    pub assets: Vec<LockedAssetOut>,
}
