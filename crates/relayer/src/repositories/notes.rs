//! The `notes` read, for replaying a chain's tree.
//!
//! Only reached on a chain fmd-indexer has written no `tree_state` row for yet;
//! see [`crate::services::tree`]. Paged by `leaf_index` rather than loaded whole,
//! so a chain with millions of notes does not hold every leaf in one query
//! result before the first hash runs.

use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::models::LeafInputsRow;
use database::schema::notes;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// The leaf inputs for `chain_id` with `leaf_index` in `[from, from + len)`,
/// in leaf order. The caller checks contiguity: a gap is a desync, not an
/// empty page.
pub async fn leaf_page(
    pool: &DbPool,
    chain_id: i64,
    from: i64,
    len: i64,
) -> AppResult<Vec<LeafInputsRow>> {
    let mut conn = super::conn(pool).await?;
    notes::table
        .filter(notes::chain_id.eq(chain_id))
        .filter(notes::leaf_index.ge(from))
        .filter(notes::leaf_index.lt(from + len))
        .order(notes::leaf_index.asc())
        .select(LeafInputsRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|e| AppError::Db(e.to_string()))
}
