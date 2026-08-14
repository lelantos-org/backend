use serde::Deserialize;
use utoipa::ToSchema;

/// Screen a single address.
///
/// A body, not a path or query parameter: `TraceLayer` records the request
/// URI, so an address in the URL would be copied into access logs and every
/// downstream log shipper. Keep it in the body.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenRequest {
    /// Address family, e.g. `evm`.
    pub chain: String,
    pub address: String,
}

/// Screen up to [`MAX_BATCH`] addresses of one family in a single round trip.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenBatchRequest {
    pub chain: String,
    pub addresses: Vec<String>,
}

/// Bounds the fan-out of one request into one `IN (…)` query.
pub const MAX_BATCH: usize = 100;
