use super::page_limit;
use crate::domain::error::{AppError, AppResult};
use serde::Deserialize;
use utoipa::IntoParams;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ListTreeAdvancesQuery {
    /// Required whenever `since_start_index` is set.
    pub chain_id: Option<i64>,
    /// Start index strictly greater than this. Page through history by feeding
    /// the previous response's maximum start index back into this field.
    pub since_start_index: Option<i64>,
    pub limit: Option<i64>,
}

impl ListTreeAdvancesQuery {
    /// Resolve `(chain_id, since_start_index, limit)` for the repository.
    ///
    /// `start_index` is unique only within a chain, and rows are ordered
    /// `(chain_id, start_index)` under one global `limit`. An unpinned cursor
    /// would skip every row of a later chain whose `start_index` sits below the
    /// cursor, so the cursor requires `chain_id`.
    pub fn page(&self) -> AppResult<(Option<i64>, Option<i64>, i64)> {
        if self.since_start_index.is_some() && self.chain_id.is_none() {
            return Err(AppError::BadRequest(
                "sinceStartIndex requires chainId: start_index is per-chain".into(),
            ));
        }
        Ok((
            self.chain_id,
            self.since_start_index,
            page_limit(self.limit),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(chain_id: Option<i64>, since_start_index: Option<i64>) -> ListTreeAdvancesQuery {
        ListTreeAdvancesQuery {
            chain_id,
            since_start_index,
            limit: None,
        }
    }

    #[test]
    fn page_accepts_a_cursor_pinned_to_a_chain() {
        let (chain_id, since, limit) = query(Some(31337), Some(7)).page().unwrap();
        assert_eq!((chain_id, since), (Some(31337), Some(7)));
        assert_eq!(limit, page_limit(None));
    }

    #[test]
    fn page_rejects_an_unpinned_cursor() {
        // Would skip rows of every chain sorting after the first.
        let err = query(None, Some(7)).page().unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)), "got {err:?}");
    }

    #[test]
    fn page_allows_an_unfiltered_first_page() {
        assert_eq!(query(None, None).page().unwrap().0, None);
    }

    #[test]
    fn page_clamps_the_limit() {
        let mut q = query(Some(1), None);
        q.limit = Some(-1);
        assert_eq!(q.page().unwrap().2, 1);
    }
}
