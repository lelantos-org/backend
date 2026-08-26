use serde::Deserialize;
use utoipa::ToSchema;

/// Screen a single address.
///
/// Carried in the body rather than a path or query parameter: `TraceLayer`
/// records the request URI, so an address in the URL would reach access logs and
/// every downstream log shipper.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenRequest {
    /// Address family, for example `evm`.
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
