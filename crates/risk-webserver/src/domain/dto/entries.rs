use serde::Deserialize;
use utoipa::IntoParams;

/// Filters for the list-entries audit endpoint.
///
/// Safe as query parameters: unlike screening, none of these values is a
/// screened address, so nothing sensitive reaches the access log.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ListEntriesQuery {
    pub chain: Option<String>,
    pub source: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

impl ListEntriesQuery {
    /// Clamp to `1..=MAX_LIMIT`. A non-positive limit falls back to the default.
    pub fn clamped_limit(&self) -> i64 {
        match self.limit {
            Some(n) if n > 0 => n.min(MAX_LIMIT),
            _ => DEFAULT_LIMIT,
        }
    }

    pub fn clamped_offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(limit: Option<i64>, offset: Option<i64>) -> ListEntriesQuery {
        ListEntriesQuery {
            chain: None,
            source: None,
            limit,
            offset,
        }
    }

    #[test]
    fn test_clamped_limit_caps_at_max() {
        assert_eq!(query(Some(10_000), None).clamped_limit(), MAX_LIMIT);
    }

    #[test]
    fn test_clamped_limit_falls_back_on_non_positive() {
        assert_eq!(query(None, None).clamped_limit(), DEFAULT_LIMIT);
        assert_eq!(query(Some(0), None).clamped_limit(), DEFAULT_LIMIT);
        assert_eq!(query(Some(-5), None).clamped_limit(), DEFAULT_LIMIT);
    }

    #[test]
    fn test_clamped_offset_is_never_negative() {
        assert_eq!(query(None, Some(-1)).clamped_offset(), 0);
        assert_eq!(query(None, Some(42)).clamped_offset(), 42);
    }
}
