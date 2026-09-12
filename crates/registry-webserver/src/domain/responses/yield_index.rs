//! The recorded yield-index history, as the wire carries it.

use serde::Serialize;
use utoipa::ToSchema;

/// One chain's history, every yield asset in one body.
///
/// Per chain rather than per asset on purpose. A client needs only the assets it
/// holds, but asking for those would tell the server which ones they are — and
/// this exists to keep a wallet's holdings out of what the server observes. One
/// body per chain is identical for every caller, so it reveals nothing and an
/// edge cache serves every wallet from one origin read.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct YieldIndexResponse {
    pub chain_id: i64,
    /// Assets with at least one recorded reading, lowest id first. An asset that
    /// has never been sampled is absent rather than present and empty.
    pub assets: Vec<YieldIndexAssetOut>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct YieldIndexAssetOut {
    pub asset_id: i64,
    /// Oldest block first, one reading per block.
    pub samples: Vec<YieldSampleOut>,
}

/// The index as it stood at one block.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct YieldSampleOut {
    pub block: i64,
    /// RAY-scaled, as a decimal string: the value exceeds what a JSON number
    /// holds exactly, the same reason `YieldOut.index` is a string.
    ///
    /// A *ratio* of two of these is what a cost basis needs, and the rounding
    /// that makes `index` display-only cancels in a ratio. Do not size an
    /// allowance with it.
    pub index_ray: String,
}
