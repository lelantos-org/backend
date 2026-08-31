pub mod anonymity_set;
pub mod asset_flows;
pub mod assets;
pub mod locked;
pub mod pool_notes;
pub mod transactions;
pub mod tree_advances;
pub mod tx_counts;

pub use anonymity_set::AnonymitySetQuery;
pub use asset_flows::AssetFlowsQuery;
pub use assets::ListAssetsQuery;
pub use locked::LockedQuery;
pub use pool_notes::PoolNotesQuery;
pub use transactions::{RecentTxQuery, TxKindsQuery};
pub use tree_advances::ListTreeAdvancesQuery;
pub use tx_counts::TxCountsQuery;

use crate::domain::error::{AppError, AppResult};
use crate::domain::responses::TxKind;

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1_000;
const HOUR_SEC: i64 = 3_600;
const DEFAULT_RECENT_SEC: i64 = 30 * 86_400;
/// Ten years. Long enough for any window a caller means, short enough that it
/// cannot silently become "all history".
const MAX_RECENT_SEC: i64 = 10 * 365 * 86_400;

/// Resolve the row limit for the cursor-paginated list endpoints.
///
/// `clamp` rather than `min`: a negative limit reaches Diesel as `LIMIT -1`, and
/// Postgres rejects it with a driver error the caller sees as a 500.
pub fn page_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Resolve the bucket width for the time-series endpoints.
///
/// Both series are built from hourly rollups, so a bucket that is not a whole
/// number of hours would split a rollup row: the SQL rounds it down and the
/// caller receives a different width than requested. Rejected instead.
pub fn bucket_sec(bucket: Option<i64>) -> AppResult<i64> {
    let bucket = bucket.unwrap_or(HOUR_SEC);
    if bucket <= 0 || bucket % HOUR_SEC != 0 {
        return Err(AppError::BadRequest(
            "bucketSec must be a positive multiple of 3600".into(),
        ));
    }
    Ok(bucket)
}

/// Resolve the lookback for the anonymity-set recency count.
///
/// This is a *second* count over a recent slice, never a filter on the
/// all-history one: an anonymity set is every withdrawal of a denomination in
/// the pool's history, so narrowing it by time would report less cover than a
/// user actually has.
///
/// Clamped rather than rejected. Both ends matter: a non-positive lookback
/// reaches SQL as a nonsense cutoff, and an unbounded one makes the recency
/// count a duplicate of the total, which reads as "every cohort is active"
/// rather than as a window that was too wide to mean anything.
pub fn recent_sec(recent: Option<i64>) -> i64 {
    recent
        .unwrap_or(DEFAULT_RECENT_SEC)
        .clamp(1, MAX_RECENT_SEC)
}

/// Resolve the kind filter for the classified feed.
///
/// Rejected rather than ignored: a misspelled kind that widened the filter would
/// answer a different question, and the caller cannot distinguish the full feed
/// from a filter that did not apply.
pub fn tx_kind(kind: Option<&str>) -> AppResult<Option<TxKind>> {
    let Some(kind) = kind else { return Ok(None) };
    TxKind::parse(kind).map(Some).ok_or_else(|| {
        // The rejected value is quoted back. The usual cause is a plural or a
        // capital, which the list of valid kinds alone does not point out.
        let known = TxKind::ALL.map(TxKind::as_str).join(", ");
        AppError::BadRequest(format!("unknown kind '{kind}'; must be one of {known}"))
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
    fn recent_sec_defaults_to_thirty_days() {
        assert_eq!(recent_sec(None), DEFAULT_RECENT_SEC);
    }

    #[test]
    fn recent_sec_rejects_a_non_positive_lookback() {
        // Reaches SQL as a cutoff in the future, which would report every
        // cohort dormant.
        assert_eq!(recent_sec(Some(0)), 1);
        assert_eq!(recent_sec(Some(-86_400)), 1);
    }

    #[test]
    fn recent_sec_caps_a_lookback_that_would_mean_all_history() {
        // Unbounded, `recentCount` duplicates `count` and every cohort reads as
        // active — worse than a window that is honestly too wide.
        assert_eq!(recent_sec(Some(i64::MAX)), MAX_RECENT_SEC);
    }

    #[test]
    fn recent_sec_passes_an_in_range_window_through() {
        assert_eq!(recent_sec(Some(7 * 86_400)), 7 * 86_400);
    }

    #[test]
    fn tx_kind_absent_means_every_kind() {
        assert_eq!(tx_kind(None).unwrap(), None);
    }

    #[test]
    fn tx_kind_accepts_every_wire_spelling() {
        for kind in TxKind::ALL {
            assert_eq!(tx_kind(Some(kind.as_str())).unwrap(), Some(kind));
        }
    }

    #[test]
    fn tx_kind_rejects_an_unknown_kind() {
        // Must not widen to the whole feed; see `tx_kind`.
        for bad in ["", "Deposit", "deposits", "sideways"] {
            let err = tx_kind(Some(bad)).unwrap_err();
            assert!(matches!(err, AppError::BadRequest(_)), "{bad} -> {err:?}");
        }
    }

    #[test]
    fn tx_kind_names_the_value_it_rejected() {
        // The caller must be able to see which parameter was wrong.
        let AppError::BadRequest(msg) = tx_kind(Some("deposits")).unwrap_err() else {
            panic!("expected a bad request");
        };
        assert!(msg.contains("deposits"), "{msg}");
        assert!(msg.contains("withdraw"), "{msg}");
    }
}
