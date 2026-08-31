use serde::Serialize;
use utoipa::ToSchema;

/// One yield-bearing asset's venue binding and last polled state.
///
/// Two groups of field, which fail independently. `venue_hex`, `buffer_bps`,
/// `perf_bps` and `halted` are event-sourced and always present. Everything from
/// `gross` down is polled from `yieldState` and is `null` together, for an asset
/// bound but not yet reached; `updated_at` says when the values that are present
/// were read, so a stale row is visible rather than silently current.
///
/// Amounts split into two units and the names say which. `gross`, `idle` and
/// `accrued_fee` are **underlying whole tokens**. `total_normalized` and
/// `accrued_fee_normalized` are **normalized units** — the pool's internal
/// share unit, not tokens — and must never be rendered as an amount of the
/// asset.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct YieldAssetOut {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    pub token_hex: String,
    /// ERC20 symbol, `null` until the indexer has read it. Clients fall back to
    /// the address rather than a synthesised label.
    pub symbol: Option<String>,
    /// The venue this asset's custody earns in, lowercase hex without `0x`.
    pub venue_hex: String,
    /// Share of the asset the pool targets holding outside the venue, in basis
    /// points, so withdrawals need not unwind a position. A real `0` is a valid
    /// configuration and arrives as `0`.
    pub buffer_bps: i16,
    /// The protocol's cut of yield earned, in basis points.
    pub perf_bps: i16,
    /// Whether accrual is halted for this asset. The binding survives a halt —
    /// the contract has no event that unbinds a venue — so a halted asset is
    /// still listed here.
    pub halted: bool,
    /// Everything backing the asset, in whole tokens: the venue position plus
    /// `idle`. This is the balance, and it is what `/v1/locked` reports for this
    /// asset under `venueHoldings`. `null` while unpolled, or while the token's
    /// decimals are unresolved.
    pub gross: Option<String>,
    /// The part of `gross` held outside the venue, in whole tokens. Compare
    /// against `buffer_bps` of `gross` to see whether the buffer is on target.
    pub idle: Option<String>,
    /// The treasury's earned-but-unswept fee, in whole tokens.
    ///
    /// Converted by the contract's own arithmetic rather than from `index_ray`;
    /// see `services::asset_yield::fee_underlying`. `null` when unpolled, when
    /// decimals are unresolved, or when nothing has been minted yet — a supply of
    /// zero has no conversion, which is not the same as a fee of zero.
    pub accrued_fee: Option<String>,
    /// Normalized units owed to note holders. **Not tokens.** With
    /// `accrued_fee_normalized` this is the supply the conversion divides by.
    pub total_normalized: Option<String>,
    /// The treasury's unswept normalized units. **Not tokens** — `accrued_fee`
    /// is the same quantity in tokens.
    pub accrued_fee_normalized: Option<String>,
    /// The conversion rate scaled by RAY (1e27), as a decimal string: it exceeds
    /// both `i64` and JSON's exact-integer range.
    ///
    /// For display and charts only. Rebuilding an amount from it reintroduces a
    /// rounding step the contract does not take.
    pub index_ray: Option<String>,
    /// Block the polled values were read at.
    pub block_number: Option<i64>,
    /// When the poll landed, as a unix timestamp, so a client can age the row.
    pub updated_at: Option<i64>,
}
