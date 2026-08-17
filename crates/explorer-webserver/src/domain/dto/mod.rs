pub mod asset_flows;
pub mod assets;
pub mod transactions;
pub mod tree_advances;
pub mod tx_counts;

pub use asset_flows::AssetFlowsQuery;
pub use assets::ListAssetsQuery;
pub use transactions::{RecentTxQuery, TxKindsQuery};
pub use tree_advances::ListTreeAdvancesQuery;
pub use tx_counts::TxCountsQuery;

use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::TxKind;

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1_000;
const HOUR_SEC: i64 = 3_600;

/// Resolve the row limit for the cursor-paginated list endpoints.
///
/// `clamp`, not `min`: a negative limit reaches Diesel as `LIMIT -1`, and
/// Postgres rejects it with a driver error the caller sees as a 500.
pub fn page_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Resolve the bucket width for the time-series endpoints.
///
/// Both series are built from hourly rollups, so a bucket that is not a whole
/// number of hours would slice a rollup row in half — the SQL rounds it down
/// and the caller silently gets a different width than requested. Reject it
/// instead.
pub fn bucket_sec(bucket: Option<i64>) -> AppResult<i64> {
    let bucket = bucket.unwrap_or(HOUR_SEC);
    if bucket <= 0 || bucket % HOUR_SEC != 0 {
        return Err(AppError::BadRequest(
            "bucketSec must be a positive multiple of 3600".into(),
        ));
    }
    Ok(bucket)
}

/// Resolve the kind filter for the classified feed.
///
/// Rejected rather than ignored: a misspelled kind that silently widened the
/// filter would answer a question the caller did not ask, and the caller has
/// no way to tell the full feed from a filter that did not apply.
pub fn tx_kind(kind: Option<String>) -> AppResult<Option<TxKind>> {
    let Some(kind) = kind else { return Ok(None) };
    TxKind::parse(&kind).map(Some).ok_or_else(|| {
        AppError::BadRequest("kind must be one of deposit, pending, transfer, withdraw".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_limit_applies_the_default() {
        assert_eq!(page_limit(None), DEFAULT_LIMIT);
    }

    #[test]
    fn page_limit_caps_the_upper_bound() {
        assert_eq!(page_limit(Some(MAX_LIMIT * 10)), MAX_LIMIT);
    }

    #[test]
    fn page_limit_rejects_a_non_positive_limit() {
        // `LIMIT -1` / `LIMIT 0` must never reach Diesel.
        assert_eq!(page_limit(Some(-1)), 1);
        assert_eq!(page_limit(Some(0)), 1);
    }

    #[test]
    fn page_limit_passes_an_in_range_value_through() {
        assert_eq!(page_limit(Some(42)), 42);
    }

    #[test]
    fn bucket_sec_defaults_to_one_hour() {
        assert_eq!(bucket_sec(None).unwrap(), HOUR_SEC);
    }

    #[test]
    fn bucket_sec_accepts_whole_hours() {
        assert_eq!(bucket_sec(Some(6 * HOUR_SEC)).unwrap(), 6 * HOUR_SEC);
        assert_eq!(bucket_sec(Some(86_400)).unwrap(), 86_400);
    }

    #[test]
    fn bucket_sec_rejects_partial_hours_and_non_positive() {
        for bad in [0, -HOUR_SEC, 1, HOUR_SEC + 1] {
            let err = bucket_sec(Some(bad)).unwrap_err();
            assert!(matches!(err, AppError::BadRequest(_)), "{bad} -> {err:?}");
        }
    }

    #[test]
    fn tx_kind_absent_means_every_kind() {
        assert_eq!(tx_kind(None).unwrap(), None);
    }

    #[test]
    fn tx_kind_accepts_every_wire_spelling() {
        for kind in [
            TxKind::Deposit,
            TxKind::Pending,
            TxKind::Transfer,
            TxKind::Withdraw,
        ] {
            assert_eq!(
                tx_kind(Some(kind.as_str().to_string())).unwrap(),
                Some(kind)
            );
        }
    }

    #[test]
    fn tx_kind_rejects_an_unknown_kind() {
        // Not silently widened to the whole feed: see `tx_kind`.
        for bad in ["", "Deposit", "sideways"] {
            let err = tx_kind(Some(bad.to_string())).unwrap_err();
            assert!(matches!(err, AppError::BadRequest(_)), "{bad} -> {err:?}");
        }
    }
}
