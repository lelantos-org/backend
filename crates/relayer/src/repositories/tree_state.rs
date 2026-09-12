//! The `tree_state` read, for bootstrapping a chain's mirror.
//!
//! fmd-indexer owns this row and advances it as it folds leaves; the relayer and
//! fmd-webserver only read it. It holds a frontier, which is an append-only
//! tree's complete resume state, so the mirror can start from a kilobyte instead
//! of replaying every `notes` row.

use crate::domain::error::{AppError, AppResult};
use database::DbPool;
use database::models::TreeStateRow;
use database::schema::tree_state;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

/// The stored tree state for `chain_id`, or `None` on a chain the indexer has
/// not written yet.
pub async fn load(pool: &DbPool, chain_id: i64) -> AppResult<Option<TreeStateRow>> {
    let mut conn = super::conn(pool).await?;
    tree_state::table
        .filter(tree_state::chain_id.eq(chain_id))
        .select(TreeStateRow::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|e| AppError::Db(e.to_string()))
}
