pub mod head;
pub mod matches;
pub mod notes;
pub mod subscriptions;
pub mod tree;

pub use head::HeadQuery;
pub use matches::ListMatchesQuery;
pub use notes::ListNotesQuery;
pub use subscriptions::CreateSubscription;
pub use tree::TreeStateQuery;

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1_000;

/// Resolve `(after, limit)` for the cursor-paginated list endpoints.
///
/// `clamp`, not `min`: a negative limit reaches Diesel as `LIMIT -1`, and
/// Postgres rejects it with a driver error that used to be echoed straight
/// back to the caller.
pub fn page(after: Option<i64>, limit: Option<i64>) -> (i64, i64) {
    (
        after.unwrap_or(0),
        limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_applies_defaults() {
        assert_eq!(page(None, None), (0, DEFAULT_LIMIT));
    }

    #[test]
    fn page_caps_the_upper_bound() {
        assert_eq!(page(Some(7), Some(MAX_LIMIT * 10)).1, MAX_LIMIT);
    }

    #[test]
    fn page_rejects_a_non_positive_limit() {
        // `LIMIT -1` / `LIMIT 0` must never reach Diesel.
        assert_eq!(page(None, Some(-1)).1, 1);
        assert_eq!(page(None, Some(0)).1, 1);
    }

    #[test]
    fn page_passes_the_cursor_through() {
        assert_eq!(page(Some(42), Some(10)), (42, 10));
    }
}
