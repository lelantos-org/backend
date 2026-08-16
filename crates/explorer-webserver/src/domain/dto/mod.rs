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
}
