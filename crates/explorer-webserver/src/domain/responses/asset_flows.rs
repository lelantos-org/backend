use serde::Serialize;
use utoipa::ToSchema;

/// Per-bucket aggregated flow.
///
/// There is no cross-asset token total. Amounts of different assets are not
/// addable in base units, nor in circuit units, since `scale` is a circuit
/// capacity parameter rather than a decimals normalizer (circuit units per whole
/// token is `10^decimals / scale`, which is 1e8 for an 18-decimal token at scale
/// 1e10 but 1e6 for a 6-decimal token at scale 1). USD is the only meaningful
/// aggregate. Therefore:
///
/// - `in` and `out` are whole-token decimal strings, present only when exactly
///   one asset is in scope across the whole response; pin one with `assetIdU64`.
///   Otherwise `null`.
/// - `in_usd` and `out_usd` convert each asset at its own decimals and price
///   before summing, and cover only the assets that could be priced.
///   `unpriced_assets` counts the rest, distinguishing a complete total from a
///   partial one. `null` means nothing in the bucket had a price.
///
/// The price is the current spot price applied to every bucket in the range, so
/// a 90-day window values three-month-old volume at today's price. These figures
/// are today's dollar worth rather than value at the time, and clients should
/// label them as such.
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
