use serde::Serialize;
use utoipa::ToSchema;

/// Per-bucket aggregated flow.
///
/// **There is no cross-asset token total.** Token amounts of different assets
/// are not addable — not in base units, and not in circuit units either, since
/// `scale` is a circuit capacity parameter rather than a decimals normalizer
/// (`circuit units per whole token = 10^decimals / scale`, which is 1e8 for an
/// 18-decimal token at scale 1e10 but 1e6 for a 6-decimal token at scale 1).
/// USD is the only meaningful aggregate.
///
/// So:
/// - `in`/`out` are **whole-token** decimal strings, present only when exactly
///   one asset is in scope across the whole response — pin one with
///   `assetIdU64`. Otherwise `null`.
/// - `in_usd`/`out_usd` convert each asset at its own price and decimals
///   before summing, and cover **only the assets that could be priced**.
///   `unpriced_assets` counts the rest, so a client can tell a complete total
///   from a partial one. `null` means nothing in the bucket had a price.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlowPoint {
    pub ts: i64,
    #[serde(rename = "in")]
    pub in_amount: Option<String>,
    #[serde(rename = "out")]
    pub out_amount: Option<String>,
    pub in_usd: Option<f64>,
    pub out_usd: Option<f64>,
    /// Assets contributing to this bucket that had no usable price.
    pub unpriced_assets: i64,
}
