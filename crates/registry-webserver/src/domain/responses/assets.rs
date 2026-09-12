//! The asset catalog, as `/v1/assets` publishes it.
//!
//! The union of what the relayer's `/chains.tokens` and explorer-webserver's
//! `/v1/assets` each used to carry, so neither consumer loses a field when both
//! repoint here. The spelling is the wallet-facing one — `assetId`, `token` —
//! rather than explorer's `assetIdU64`/`tokenHex`: publishing both would make
//! every future column land twice, which is the duplication this route exists to
//! end.

use serde::Serialize;
use utoipa::ToSchema;

/// One asset a wallet may hold on a chain.
///
/// Carries the label and decimals so a client can render an amount without a
/// per-token `symbol()` and `decimals()` round trip of its own.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetOut {
    pub chain_id: i64,
    /// MASP asset id, as used in circuit inputs.
    pub asset_id: i64,
    /// 0x-prefixed ERC-20 address.
    pub token: String,
    /// Circuit capacity parameter (`baseUnits / scale` must fit `uint48`), not a
    /// decimals normalizer. A decimal string, since it exceeds `u53`.
    pub scale: String,
    /// Absent until the indexer has read it: unknown, not 18.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimals: Option<i16>,
    /// Absent until the indexer has read it, or when the token implements no
    /// `symbol()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Protocol fee on a shield of this asset, in bps. Absent until an
    /// `AssetFeeSet` has been indexed.
    ///
    /// Rates are per asset and per leg — there is no pool-wide fee — so an
    /// absent value is unknown, not zero, and the two legs differ routinely.
    /// The deposit rate is charged **on top** of the principal, while the
    /// withdraw rate is **skimmed from** the proceeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deposit_bps: Option<i16>,
    /// Protocol fee on an unshield of this asset, in bps. See `depositBps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withdraw_bps: Option<i16>,
    /// Present iff this asset's custody earns in a venue. Absent means plain
    /// custody, where one circuit unit is worth `scale` base units forever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yield_state: Option<YieldOut>,
}

/// What a yield-bearing asset is currently worth, and under what terms.
///
/// A client needs this to size a shield: the pull is
/// `ceil(units * gross / supply)`, and `Permit2.maxTotal` is signed over that
/// figure. Quoting at `scale` instead under-signs the allowance by whatever the
/// venue has earned, and the pull reverts.
///
/// `gross` and `supply` are published rather than only `index`, because that is
/// how the pool converts — `scale` and `RAY` cancel out of `units * gross /
/// supply`. Converting through the rounded index instead lands a unit or two
/// from what the contract charges, which at the boundary is the difference
/// between a pull that fits the signed ceiling and one that does not. `index` is
/// for display.
///
/// Absent entirely until the indexer's first poll lands: an asset known to be
/// yield-bearing but not yet priced must not be quoted at `scale`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct YieldOut {
    /// 0x-prefixed venue address. Bound once at registration and immutable.
    pub venue: String,
    /// Venue position plus the pool's idle balance, in base units.
    pub gross: String,
    /// Units outstanding, note holders plus the treasury's unswept fee.
    pub supply: String,
    /// `gross * RAY / (supply * scale)`, for display. Do not convert with it.
    pub index: String,
    /// The venue is no longer being supplied. Existing backing is unaffected —
    /// the asset degrades to zero-yield custody, still fully backed.
    pub halted: bool,
    /// Estimated annual rate for a note holder, in basis points, net of the
    /// pool's performance fee and idle buffer.
    ///
    /// An **estimate**, measured over `apyWindowS` and annualized on the
    /// assumption it continues — not a promise, and not what any particular
    /// wallet earned, which depends on when it bought in.
    ///
    /// Absent, never zero, when it could not be measured or has stopped being
    /// refreshed: an RPC without state that far back, a venue younger than the
    /// window, or a reading too wild to be a rate. A client must render nothing
    /// rather than `0.00%`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apy_bps: Option<i32>,
    /// Seconds actually spanned by the two readings behind `apyBps`. Present iff
    /// `apyBps` is.
    ///
    /// Published rather than assumed: a client that says "over the last week"
    /// while the measurement spanned nine days is stating something nobody
    /// measured, and the window is what makes the figure checkable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apy_window_s: Option<i64>,
    /// The `name()` of the ERC-4626 vault behind `venue`, as the vault reports
    /// it on chain — not a curated label. What tells an earning asset apart from
    /// the plain asset sharing its token and symbol.
    ///
    /// Absent until the indexer has read it, or for a vault without `name()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_name: Option<String>,
}
