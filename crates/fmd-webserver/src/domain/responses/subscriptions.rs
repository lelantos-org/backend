use serde::Serialize;
use utoipa::ToSchema;

/// Returned from `POST /v1/subscriptions`. Neither the token nor the detection
/// key is echoed, since the caller supplies both. The detection key is a per-user
/// secret and the notes it matches are immutable on chain, so a leaked key cannot
/// be rotated retroactively.
///
/// `created` distinguishes a fresh registration from a re-attach to an existing
/// subscription under the same token; `false` means the backfill is already under
/// way or complete.
#[derive(Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionOut {
    pub gamma: i32,
    pub active: bool,
    pub created: bool,
}
