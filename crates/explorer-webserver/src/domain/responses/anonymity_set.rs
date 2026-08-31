use serde::Serialize;
use utoipa::ToSchema;

/// One withdrawal denomination and the cohort that published it.
///
/// The set is actual, not potential: `count` is how many withdrawals of this
/// denomination the pool has seen, not how many it could support. A `count` of
/// one is a withdrawal with no cover at all, which is the case a UI most needs
/// to surface.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnonymitySetOut {
    pub chain_id: i64,
    pub asset_id_u64: i64,
    /// The published circuit value, as a decimal string.
    ///
    /// A string rather than a number: the underlying `uint64` exceeds both
    /// `i64::MAX` and the exact-integer range of a JSON double, so parsing it as
    /// a number could round two distinct denominations together. Consumers must
    /// treat it as an opaque key, not convert it — the whole-token value it
    /// represents moves with the yield index while the denomination does not.
    pub public_out: String,
    /// Withdrawals that published this denomination, over all history. The k in
    /// k-anonymity.
    ///
    /// An **upper bound** on cover, not a headcount: it counts withdrawals, not
    /// distinct users, and one person exiting repeatedly at one denomination is
    /// indistinguishable here from that many separate users. Telling them apart
    /// would need recipient addresses, which this service does not index.
    pub count: i64,
    /// How many of those fell inside the caller's `recentSec` lookback.
    ///
    /// A subset of `count`, never a replacement for it. Cover shared with
    /// nobody recently is cover that may no longer be there — a denomination
    /// abandoned a year ago still carries its full historical `count`. Zero
    /// means dormant, and is a fact about the window, not about the cohort's
    /// size.
    pub recent_count: i64,
    /// First and last time this denomination was published. A wide span on a
    /// small count means the cohort is spread thin, not that it is active.
    pub first_ts: i64,
    pub last_ts: i64,
}
